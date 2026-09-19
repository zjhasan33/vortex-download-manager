// Vortex Companion — content script.
// Detects video/audio elements + media URLs on the page and offers
// "Download with Vortex" via a floating button (like IDM's extension).

(function () {
  if (window.__vortexContentInstalled) return;
  window.__vortexContentInstalled = true;

  // Dual-browser support (see background.js): alias the Firefox `browser.*`
  // namespace for Chrome so the same code runs on both.
  if (typeof browser === "undefined" && typeof globalThis.chrome !== "undefined") {
    var browser = globalThis.chrome;
  }

  const MEDIA_EXT = ["mp4", "webm", "mov", "m4v", "mkv", "flv", "m4a", "mp3", "ogg", "oga", "opus", "wav", "aac", "flac", "m3u8"];
  const MEDIA_SITES = ["youtube.com", "youtu.be", "youtube-nocookie.com", "tiktok.com", "instagram.com", "facebook.com", "fb.watch", "twitter.com", "x.com", "dailymotion.com", "vimeo.com", "soundcloud.com", "bilibili.com", "twitch.tv", "reddit.com"];

  // SPA-proof current URL — NEVER cache location.href: YouTube navigates
  // (watch→watch, Shorts scroll) without reloading, so a cached URL keeps
  // pointing at the previous video. Evaluate on demand, every time.
  function getCurrentUrl() {
    try {
      const path = location.pathname || "";
      const pm = path.match(/^\/shorts\/([\w-]{6,})/);
      if (pm) return "https://www.youtube.com/shorts/" + pm[1];
      // Shorts scrolled but URL lagging: use the reel actually in viewport.
      const reels = document.querySelectorAll("ytd-reel-video-renderer");
      for (const r of reels) {
        const rect = r.getBoundingClientRect();
        if (!rect || rect.width <= 0) continue;
        const vis = Math.min(rect.bottom, window.innerHeight) - Math.max(rect.top, 0);
        if (vis > 80) {
          const a = r.querySelector('a[href*="/shorts/"]');
          const href = a && a.getAttribute("href");
          const m = href && href.match(/\/shorts\/([\w-]{6,})/);
          if (m) return "https://www.youtube.com/shorts/" + m[1];
        }
      }
    } catch (e) { /* fall through to location */ }
    return location.href;
  }

  function hostOf(url) {
    try { return new URL(url).hostname.toLowerCase(); } catch (e) { return ""; }
  }
  function onlyHost(h) {
    return MEDIA_SITES.some((s) => h === s || h.endsWith("." + s));
  }
  function extOf(url) {
    try {
      // Strip query/hash first so "file.txt?key=123" can't bypass extension filters.
      const clean = String(url).split("?")[0].split("#")[0];
      const p = new URL(clean).pathname.toLowerCase();
      const m = p.match(/\.([a-z0-9]{2,5})$/);
      return m ? m[1] : "";
    } catch (e) { return ""; }
  }
  function fileName(u) {
    try {
      const n = decodeURIComponent(new URL(u).pathname.split("/").pop() || "");
      return n || new URL(u).hostname;
    } catch (e) {
      return u.split("/").pop() || u;
    }
  }
  function fmtBytes(n) {
    if (!n) return "";
    if (n > 1073741824) return (n / 1073741824).toFixed(2) + " GB";
    if (n > 1048576) return (n / 1048576).toFixed(1) + " MB";
    if (n > 1024) return (n / 1024).toFixed(0) + " kB";
    return n + " B";
  }
  function esc(s) {
    return String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  }

  const media = new Map(); // url -> {url, title, kind, size}
  const listeners = [];
  let panelOpen = false;
  let captureToast = null;
  let fetchBusy = false;
  let hbar = null; // IDM-style hover bar over a <video>
  let hbarTarget = null;
  let hbarHideT = 0;
  let hbarFmts = null; // formats/subs dropdown
  let fmtsOpen = false;
  let hbarSubFmt = "srt"; // standalone subtitle output format
  const HBAR_POS_KEY = "vx_hbar_pos";
  const HBAR_OPACITY_KEY = "vx_hbar_opacity";
  let hbarPos = null; // {x, y} — last dragged position (persisted)
  let hbarOpacity = 100; // hover bar opacity % (persisted, popup slider)
  function applyHbarOpacity() {
    const o = String(Math.min(100, Math.max(20, Number(hbarOpacity) || 100)) / 100);
    if (hbar) hbar.style.opacity = o;
    if (hbarFmts) hbarFmts.style.opacity = o;
  }
  browser.storage.local.get({ [HBAR_OPACITY_KEY]: 100 }).then((r) => {
    hbarOpacity = r[HBAR_OPACITY_KEY];
    applyHbarOpacity();
  });
  browser.storage.onChanged.addListener((changes, area) => {
    if (area === "local" && changes[HBAR_OPACITY_KEY]) {
      hbarOpacity = changes[HBAR_OPACITY_KEY].newValue;
      applyHbarOpacity();
    }
  });
  let isDragging = false; // a drag is in progress: placeHbar() must not move the bar
  let dragOffsetX = 0; // grab offset so the bar doesn't jump to the cursor
  let dragOffsetY = 0;
  const boundBarVideos = new WeakSet();
  browser.storage.local.get({ [HBAR_POS_KEY]: null }).then((r) => {
    const p = r[HBAR_POS_KEY];
    if (p && typeof p === "object" && typeof p.x === "number" && typeof p.y === "number") {
      hbarPos = p;
    }
  });

  function clamp(v, lo, hi) {
    return Math.max(lo, Math.min(hi, v));
  }

  function el(tag, cls, text) {
    const n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text != null) n.textContent = text;
    return n;
  }

  // ---------------- detection ----------------

  function register(el, src, kind) {
    if (!src) return;
    let url = "";
    if (typeof src === "string") url = src.startsWith("/") ? location.origin + src : src;
    else if (src instanceof URL) url = src.href;
    if (!url || !/^https?:/.test(url)) return;
    if (media.has(url)) {
      const m = media.get(url);
      if (m.kind === "media" && kind === "media") return;
    }
    media.set(url, { url, title: fileName(url), kind, size: 0 });
    if (el && el.duration && !isNaN(el.duration) && el.duration > 0) {
      media.get(url).title = String(document.title || fileName(url));
    }
  }

  function scan() {
    for (const v of document.querySelectorAll("video, audio")) {
      const src = v.currentSrc || v.src;
      if (src) {
        const e = extOf(src);
        if (/^https?:/.test(src) && MEDIA_EXT.includes(e)) {
          register(v, src, e === "m3u8" ? "hls" : "media");
        } else if (v.duration && !isNaN(v.duration) && (v.readyState >= 1 || !v.paused)) {
          register(v, getCurrentUrl(), "page");
        }
      } else {
        for (const s of v.querySelectorAll("source[src]")) {
          const e = extOf(s.src);
          if (MEDIA_EXT.includes(e)) register(v, s.src, e === "m3u8" ? "hls" : "media");
        }
      }
    }
    // youtube-like: video uses blob: — mark page-level
    if (onlyHost(hostOf(getCurrentUrl())) && document.querySelector("video")) {
      if (!media.has("__page__")) media.set("__page__", { url: getCurrentUrl(), title: "This page's video", kind: "page" });
    }
    bindBarVideos();
  }

  function refreshUI() {
    if (!panelOpen) return;
    renderPanel();
  }

  // ---------------- browser messaging ----------------

  function send(msg) {
    return browser.runtime.sendMessage(msg).then(
      (res) => res || {},
      (e) => ({ error: (e && e.message) || String(e) })
    );
  }

  // ---------------- UI ----------------

  let fab, panel;

  function ensureEls() {
    if (fab) return;
    fab = el("div", "vx-fab");
    fab.appendChild(el("span", "vx-logo"));
    fab.appendChild(document.createTextNode("Vortex"));
    fab.addEventListener("click", togglePanel);
    document.documentElement.appendChild(fab);

    panel = el("div", "vx-panel vx-hidden");
    panel.innerHTML =
      '<div class="vx-panel-head"><span class="vx-title"></span><button class="vx-x">✕</button></div>' +
      '<div class="vx-panel-body"></div>';
    panel.querySelector(".vx-x").addEventListener("click", hidePanel);
    document.documentElement.appendChild(panel);
  }

  function togglePanel() {
    if (panelOpen) hidePanel();
    else {
      panelOpen = true;
      ensureEls();
      panel.classList.remove("vx-hidden");
      if (hbar) hbar.classList.add("vx-hide");
      scan();
      renderPanel();
    }
  }
  function hidePanel() {
    panelOpen = false;
    if (panel) panel.classList.add("vx-hidden");
  }

  function nameOf(m) {
    return m.kind === "page" ? "This page's video — " + m.title : m.title;
  }

  async function renderPanel() {
    const body = panel.querySelector(".vx-panel-body");
    const host = hostOf(getCurrentUrl());
    const isVa = onlyHost(host);

    panel.querySelector(".vx-title").textContent = (document.title || host).slice(0, 60);

    let html = "";
    if (isVa || media.has("__page__")) {
      html += '<button class="vx-primary" id="vx-fetch">' + (fetchBusy ? '<span class="vx-spin"></span> Fetching formats…' : "▼ Fetch available formats") + "</button>";
    }
    body.innerHTML = html;

    const fetchBtn = body.querySelector("#vx-fetch");
    if (fetchBtn) {
      fetchBtn.addEventListener("click", async () => {
        if (fetchBusy) return;
        fetchBusy = true;
        fetchBtn.innerHTML = '<span class="vx-spin"></span> Fetching formats…';
        const url = getCurrentUrl();
        const res = await send({ type: "analyze", url });
        fetchBusy = false;
        if (res && res.info && (res.info.formats || []).length) {
          const list = res.info.formats;
          let html2 = "";
          for (const f of list) {
            html2 +=
              '<div class="vx-fmt"><span class="vx-fq">' + esc(f.label) +
              '<span class="vx-fn">' + (f.size ? " • " + fmtBytes(f.size) : "") + "</span></span>" +
              '<button class="vx-btn vx-slim" data-fid="' + esc(f.id) + '">Download</button></div>';
          }
          const subs = (res.info.subtitles || []).filter((s) => !s.auto).slice(0, 15);
          if (subs.length) {
            html2 += '<div class="vx-empty" style="text-align:left;padding:8px 2px 4px">Subtitles / Captions</div>';
            for (const s of subs) {
              html2 +=
                '<div class="vx-fmt"><span class="vx-fq">' + esc(s.label) +
                "</span><button class='vx-btn vx-slim' data-sub='" + esc(s.lang) + "'>SRT</button></div>";
            }
          }
          body.innerHTML = html2;
          body.querySelectorAll("[data-fid]").forEach((btn) => {
            btn.addEventListener("click", () => {
              btn.textContent = "Added ✓";
              btn.disabled = true;
              void send({ type: "start_ytdl", url, format_id: btn.dataset.fid });
            });
          });
          body.querySelectorAll("[data-sub]").forEach((btn) => {
            btn.addEventListener("click", () => {
              btn.textContent = "Added ✓";
              btn.disabled = true;
              void send({ type: "start_ytdl", url, format_id: "subs:srt:" + btn.dataset.sub });
            });
          });
        } else if (res && res.info && (res.info.subtitles || []).some((s) => !s.auto)) {
          let html2 = "";
          for (const s of res.info.subtitles.filter((s) => !s.auto).slice(0, 15)) {
            html2 +=
              '<div class="vx-fmt"><span class="vx-fq">' + esc(s.label) +
              "</span><button class='vx-btn vx-slim' data-sub='" + esc(s.lang) + "'>SRT</button></div>";
          }
          body.innerHTML = html2;
          body.querySelectorAll("[data-sub]").forEach((btn) => {
            btn.addEventListener("click", () => {
              btn.textContent = "Added ✓";
              btn.disabled = true;
              void send({ type: "start_ytdl", url, format_id: "subs:srt:" + btn.dataset.sub });
            });
          });
        } else {
          body.innerHTML =
            '<div class="vx-empty">Error: ' + esc((res && res.error) || "no info") + '<br/><span style="font-size:11px">Open Vortex, then retry.</span></div>';
        }
      });
    }

    const items = [...media.values()].filter((m) => m.kind !== "page" && !/^blob:/i.test(m.url));
    if (items.length) {
      body.insertAdjacentHTML("beforeend", '<div class="vx-empty" style="text-align:left;padding:8px 2px 4px">Direct media on this page</div>');
      for (const m of items) {
        const row = el("div", "vx-row");
        const info = el("div", "vx-info");
        info.appendChild(el("div", "vx-fname", m.title.length > 40 ? m.title.slice(0, 37) + "…" : m.title));
        info.appendChild(el("div", "vx-k", (m.kind === "hls" ? "HLS stream" : "media") + " • " + m.url.split("://")[1].split("/")[0]));
        const btn = el("button", "vx-btn vx-slim", m.kind === "hls" ? "Download" : "Download");
        btn.addEventListener("click", () => {
          btn.textContent = "Added ✓";
          btn.disabled = true;
          if (m.kind === "hls") {
            // HLS → route through yt-dlp auto-best
            void send({ type: "analyze", url: m.url }).then((res) => {
              const f = res && res.info && res.info.formats && res.info.formats.find((x) => x.has_video && x.has_audio);
              if (f) void send({ type: "start_ytdl", url: m.url, format_id: f.id });
              else void send({ type: "download_direct", url: m.url, filename: m.title });
            });
          } else {
            void send({ type: "download_direct", url: m.url, filename: m.title });
          }
        });
        row.appendChild(info);
        row.appendChild(btn);
        body.appendChild(row);
      }
    }

    if (!body.querySelector(".vx-primary") && !items.length) {
      body.insertAdjacentHTML("beforeend", '<div class="vx-empty">No video/media detected on this page.</div>');
    }
    body.insertAdjacentHTML(
      "beforeend",
      '<div class="vx-empty" style="padding:10px 4px 4px"><a href="#" id="vx-openapp" style="color:#00e5ff;text-decoration:none;font-size:12px">Open Vortex app</a></div>'
    );
    const opener = body.querySelector("#vx-openapp");
    if (opener) opener.addEventListener("click", (e) => { e.preventDefault(); void send({ type: "open_vortex" }); });
  }

  function showToast(cap) {
    if (!captureToast) {
      ensureEls();
      captureToast = el("div", "vx-toast vx-hide");
      document.documentElement.appendChild(captureToast);
    }
    const name = (cap && cap.filename) || "";
    captureToast.classList.remove("vx-hide");
    captureToast.innerHTML =
      "<div><b style='color:#00e5ff'>Vortex:</b> link detected<br/><span class='vx-t-name'>" + esc(name) + "</span></div>" +
      '<button class="vx-btn vx-slim">Download</button>';
    const b = captureToast.querySelector("button");
    b.onclick = () => {
      if (cap) void send({ type: "download_direct", url: cap.url, filename: cap.filename });
      captureToast.classList.add("vx-hide");
    };
    clearTimeout(captureToast._t);
    captureToast._t = setTimeout(() => captureToast.classList.add("vx-hide"), 9000);
  }

  // ---------------- IDM-style hover bar over the video ----------------

  function mediaSite() {
    return onlyHost(hostOf(getCurrentUrl()));
  }

  function ensureHbarEls() {
    if (hbar) return;
    hbar = el("div", "vx-hbar vx-hide");
    hbar.addEventListener("pointerdown", (e) => e.stopPropagation());
    hbar.addEventListener("pointerenter", () => clearTimeout(hbarHideT));
    hbar.addEventListener("pointerleave", () => { if (!fmtsOpen && !isDragging) hbarHide(350); });
    document.documentElement.appendChild(hbar);

    // ---- Bulletproof drag (survives YouTube's player event capture) ----
    // Drags start ONLY on the grip / logo / label — never on buttons — and
    // mousemove/mouseup live on `window`, so the pointer can leave the bar
    // (or the player) without breaking the drag. While dragging, placeHbar()
    // is skipped, so the player/follow logic can never reset the position.
    hbar.addEventListener("mousedown", (e) => {
      if (e.button !== 0 || isDragging) return;
      if (e.target.closest("button, a, input, select")) return; // let the real buttons work
      e.preventDefault();
      e.stopPropagation();
      e.stopImmediatePropagation(); // YouTube never sees the mousedown
      const r = hbar.getBoundingClientRect();
      dragOffsetX = e.clientX - r.left;
      dragOffsetY = e.clientY - r.top;
      isDragging = true;
      hbar.classList.add("vx-hb-dragging");
    });
    window.addEventListener("mousemove", (e) => {
      if (!isDragging) return;
      e.preventDefault();
      hbar.style.left = clamp(e.clientX - dragOffsetX, 2, window.innerWidth - (hbar.offsetWidth || 150) - 2) + "px";
      hbar.style.top = clamp(e.clientY - dragOffsetY, 2, window.innerHeight - (hbar.offsetHeight || 40) - 2) + "px";
    }, true);
    window.addEventListener("mouseup", () => {
      if (!isDragging) return;
      isDragging = false;
      hbar.classList.remove("vx-hb-dragging");
      // Pinned: remember the final position so it survives re-shows + page scroll.
      const r = hbar.getBoundingClientRect();
      hbarPos = { x: Math.round(r.left), y: Math.round(r.top) };
      void browser.storage.local.set({ [HBAR_POS_KEY]: hbarPos });
    }, true);
    hbarFmts = el("div", "vx-hb-fmts vx-hide");
    hbarFmts.addEventListener("pointerenter", () => { clearTimeout(hbarHideT); fmtsOpen = true; });
    hbarFmts.addEventListener("pointerleave", () => hbarHide(350));
    hbarFmts.addEventListener("pointerdown", (e) => e.stopPropagation());
    document.documentElement.appendChild(hbarFmts);
    applyHbarOpacity();
    document.addEventListener("pointerdown", (e) => {
      if (fmtsOpen && !hbarFmts.contains(e.target) && !hbar.contains(e.target)) hbarHide(0);
    });
    document.addEventListener("keydown", (e) => {
      if (e.key === "Escape" && fmtsOpen) hbarHide(0);
    });
  }

  function makeHbarBtn(label, onClick) {
    const b = el("button", "vx-hb-btn", label);
    b.addEventListener("click", onClick);
    return b;
  }

  function barForMediaSite() {
    hbar.innerHTML = '<span class="vx-hb-grip" title="Drag to move">⋮⋮</span><span class="vx-logo"></span><span class="vx-hb-label">Download:</span>';
    hbar.appendChild(makeHbarBtn("MP4 Best", () => hbarStart("bestvideo+bestaudio/best", "MP4 Best")));
    hbar.appendChild(makeHbarBtn("720p", () => hbarStart("bestvideo[height<=720]+bestaudio/best[height<=720]", "720p")));
    hbar.appendChild(makeHbarBtn("MP3", () => hbarStart("ba-mp3-320", "MP3")));
    hbar.appendChild(makeHbarBtn("▾ Subs", () => hbarSubs()));
    hbar.appendChild(el("span", "vx-hb-sep"));
    hbar.appendChild(makeHbarBtn("▾ Formats", () => hbarFormats()));
  }

  function barForDirect(m) {
    hbar.innerHTML = '<span class="vx-hb-grip" title="Drag to move">⋮⋮</span><span class="vx-logo"></span><span class="vx-hb-label">Vortex</span>';
    hbar.appendChild(
      makeHbarBtn("Download", () => {
        const btn = hbar.querySelector(".vx-hb-btn");
        btn.disabled = true;
        btn.textContent = "…";
        void send({ type: "download_direct", url: m.url, filename: m.title }).then((res) => {
          const ok = res && (res.ok || res.launched);
          btn.textContent = ok ? "Added ✓" : "Vortex off";
          btn.classList.toggle("vx-hb-added", !!ok);
          setTimeout(() => {
            btn.textContent = "Download";
            btn.disabled = false;
            btn.classList.remove("vx-hb-added");
          }, 2500);
        });
      })
    );
  }

  function placeHbar() {
    if (!hbarTarget || !document.contains(hbarTarget)) {
      hbarHide(0);
      return;
    }
    // While a drag is in flight the bar owns its own position — the player /
    // follow logic must never fight the user's hand.
    if (isDragging) return;
    const w = hbar.offsetWidth || 150;
    // Pinned (dragged) position wins — the bar no longer follows the video,
    // but stays clamped inside the viewport.
    if (hbarPos) {
      const h = hbar.offsetHeight || 40;
      hbar.style.left = clamp(hbarPos.x, 2, window.innerWidth - w - 2) + "px";
      hbar.style.top = clamp(hbarPos.y, 2, window.innerHeight - h - 2) + "px";
      return;
    }
    const r = hbarTarget.getBoundingClientRect();
    if (r.width < 60 || r.height < 30 || r.bottom < 0 || r.top > window.innerHeight) return;
    hbar.style.left = Math.max(4, r.right - 8 - w) + "px";
    hbar.style.top = (r.top + 8) + "px";
  }

  function placeFmtsBelow() {
    if (!hbarTarget) return;
    const r = hbarTarget.getBoundingClientRect();
    const bw = hbarFmts.offsetWidth || 250;
    const bh = hbarFmts.offsetHeight || 200;
    let x = r.right - 8 - bw;
    if (x < 4) x = 4;
    let y = r.top + 40;
    if (y + bh > window.innerHeight - 8) y = Math.max(8, window.innerHeight - bh - 8);
    hbarFmts.style.left = x + "px";
    hbarFmts.style.top = y + "px";
  }

  // Exotic stream sniffer: background.js pushes the playing page's HLS/DASH
  // manifest here so the hover bar appears even with no direct file URL.
  // Freshness-guarded (60 s + same page): a stale stream never downloads.
  let pendingStream = null;
  function streamFresh(s) {
    if (!s || !s.url) return null;
    if (Date.now() - (s.at || 0) > 60000) return null;
    try {
      if (s.pageUrl && !location.href.startsWith(s.pageUrl.split("?")[0].split("#")[0])) return null;
    } catch (e) { /* compare failed: trust recency */ }
    return s;
  }

  function hbarShow(video) {
    clearTimeout(hbarHideT);
    if (!video || !document.contains(video)) return;
    const direct = findMediaForVideo(video);
    const stream = !direct ? streamFresh(pendingStream) : null;
    if (!mediaSite() && !direct && !stream) return;
    ensureHbarEls();
    hbarTarget = video;
    if (stream) barForStream(stream);
    else if (mediaSite()) barForMediaSite();
    else barForDirect(direct);
    hbar.classList.remove("vx-hide");
    placeHbar();
  }

  function hbarHide(ms) {
    clearTimeout(hbarHideT);
    if (isDragging) return; // never hide while the user is dragging
    hbarHideT = setTimeout(() => {
      if (hbar) hbar.classList.add("vx-hide");
      if (hbarFmts) hbarFmts.classList.add("vx-hide");
      fmtsOpen = false;
      hbarTarget = null;
    }, ms || 350);
  }

  function findMediaForVideo(video) {
    const src = video.currentSrc || video.src;
    if (src) {
      const norm = src.startsWith("/") ? location.origin + src : src;
      if (media.has(norm)) return media.get(norm);
    }
    for (const s of video.querySelectorAll("source[src]")) {
      if (media.has(s.src)) return media.get(s.src);
    }
    return null;
  }

  // Hover bar for a sniffed HLS/DASH stream: analyze the manifest, then
  // download best video+audio through yt-dlp (page headers replayed).
  function barForStream(stream) {
    const tag = stream.kind === "dash" ? "DASH" : "HLS";
    hbar.innerHTML = '<span class="vx-hb-grip" title="Drag to move">⋮⋮</span><span class="vx-logo"></span><span class="vx-hb-label">Download Video (' + tag + "):</span>";
    hbar.appendChild(makeHbarBtn("Download", () => hbarStartStream(stream)));
    hbar.appendChild(makeHbarBtn("▾ Formats", () => hbarFormats(stream.url)));
  }

  async function hbarStartStream(stream) {
    const btn = [...hbar.querySelectorAll(".vx-hb-btn")].find((x) => x.textContent.trim() === "Download");
    if (btn) { btn.disabled = true; btn.textContent = "…"; }
    const done = (txt, added) => {
      if (!btn) return;
      btn.classList.toggle("vx-hb-added", !!added);
      btn.textContent = txt;
      setTimeout(() => {
        btn.classList.remove("vx-hb-added");
        btn.textContent = "Download";
        btn.disabled = false;
      }, 2500);
    };
    const res = await send({ type: "analyze", url: stream.url });
    const f = res && res.info && res.info.formats && res.info.formats.find((x) => x.has_video && x.has_audio);
    if (f) {
      const r2 = await send({ type: "start_ytdl", url: stream.url, format_id: f.id });
      done(r2 && r2.ok ? "Added ✓" : String((r2 && r2.error) || "Vortex off?").slice(0, 28), r2 && r2.ok);
    } else {
      done(String((res && res.error) || "No formats").slice(0, 28), false);
    }
  }

  async function hbarStart(formatId, label2, urlOverride) {
    if (hbarFmts) hbarFmts.classList.add("vx-hide");
    const btn = [...hbar.querySelectorAll(".vx-hb-btn")].find((x) => x.textContent.trim() === label2);
    if (!btn) return;
    btn.disabled = true;
    btn.textContent = "…";
    const res = await send({ type: "start_ytdl", url: urlOverride || getCurrentUrl(), format_id: formatId });
    if (res && res.ok) {
      btn.classList.add("vx-hb-added");
      btn.textContent = "Added ✓";
    } else if (res && res.error) {
      // Real server failure (e.g. tools still fetching) — show it instead of
      // a generic message so the user knows what to fix.
      btn.textContent = String(res.error).slice(0, 28);
    } else {
      btn.textContent = "Vortex off?";
    }
    setTimeout(() => {
      btn.classList.remove("vx-hb-added");
      btn.textContent = label2;
      btn.disabled = false;
    }, 2500);
  }

  async function hbarFormats(urlOverride) {
    fmtsOpen = true;
    hbarFmts.classList.remove("vx-hide");
    hbarFmts.innerHTML = '<div class="vx-empty" style="padding:10px"><span class="vx-spin"></span> Fetching formats…</div>';
    placeFmtsBelow();
    const u = urlOverride || getCurrentUrl();
    const res = await send({ type: "analyze", url: u });
    const subs = (res && res.info && res.info.subtitles || []).filter((s) => !s.auto).slice(0, 15);
    if (!res || !res.info || !(res.info.formats || []).length) {
      if (!subs.length) {
        hbarFmts.innerHTML = '<div class="vx-empty">Error: ' + esc((res && res.error) || "no formats") + "</div>";
        return;
      }
    }
    let html = "";
    for (const f of res.info.formats || []) {
      html +=
        '<div class="vx-fmt"><span class="vx-fq">' + esc(f.label) +
        '<span class="vx-fn">' + (f.size ? " • " + fmtBytes(f.size) : "") + "</span></span>" +
        '<button class="vx-btn vx-slim" data-fid="' + esc(f.id) + '">Download</button></div>';
    }
    if (subs.length) {
      html += '<div class="vx-empty" style="text-align:left;padding:8px 2px 4px">Subtitles / Captions</div>';
      for (const s of subs) {
        html +=
          '<div class="vx-fmt"><span class="vx-fq">' + esc(s.label) +
          "</span><button class='vx-btn vx-slim' data-sub='" + esc(s.lang) + "'>SRT</button></div>";
      }
    }
    hbarFmts.innerHTML = html;
    hbarFmts.querySelectorAll("[data-fid]").forEach((btn) => {
      btn.addEventListener("click", () => {
        btn.textContent = "Added ✓";
        btn.disabled = true;
        void send({ type: "start_ytdl", url: getCurrentUrl(), format_id: btn.dataset.fid });
      });
    });
    hbarFmts.querySelectorAll("[data-sub]").forEach((btn) => {
      btn.addEventListener("click", () => {
        btn.textContent = "Added ✓";
        btn.disabled = true;
        void send({ type: "start_ytdl", url: getCurrentUrl(), format_id: "subs:srt:" + btn.dataset.sub });
      });
    });
    placeFmtsBelow();
  }

  // Dedicated "▾ Subs" dropdown: list official subtitle languages + choose SRT/VTT.
  async function hbarSubs(urlOverride) {
    fmtsOpen = true;
    hbarFmts.classList.remove("vx-hide");
    hbarFmts.innerHTML = '<div class="vx-empty" style="padding:10px"><span class="vx-spin"></span> Fetching subtitles…</div>';
    placeFmtsBelow();
    const u = urlOverride || getCurrentUrl();
    const res = await send({ type: "analyze", url: u });
    const subs = ((res && res.info && res.info.subtitles) || []).filter((s) => !s.auto);
    if (!subs.length) {
      hbarFmts.innerHTML =
        '<div class="vx-empty">' +
        (res && res.error ? "Error: " + esc(res.error) : "No official subtitles available for this video") +
        "</div>";
      placeFmtsBelow();
      return;
    }
    renderSubs(subs, u);
  }

  function renderSubs(subs, url) {
    let html =
      '<div class="vx-subfmt">' +
      '<span class="vx-subfmt-lbl">Format</span>' +
      '<button class="vx-btn vx-slim ' + (hbarSubFmt === "srt" ? "vx-on" : "") + '" data-subfmt="srt">SRT</button>' +
      '<button class="vx-btn vx-slim ' + (hbarSubFmt === "vtt" ? "vx-on" : "") + '" data-subfmt="vtt">VTT</button>' +
      "</div>";
    for (const s of subs.slice(0, 20)) {
      html +=
        '<div class="vx-fmt"><span class="vx-fq">' + esc(s.label || s.lang) +
        "</span><button class='vx-btn vx-slim' data-sub='" + esc(s.lang) + "'>." + hbarSubFmt + "</button></div>";
    }
    hbarFmts.innerHTML = html;
    hbarFmts.querySelectorAll("[data-subfmt]").forEach((btn) => {
      btn.addEventListener("click", () => {
        hbarSubFmt = btn.dataset.subfmt;
        renderSubs(subs, url);
      });
    });
    hbarFmts.querySelectorAll("[data-sub]").forEach((btn) => {
      btn.addEventListener("click", () => {
        btn.textContent = "Added ✓";
        btn.disabled = true;
        void send({ type: "start_ytdl", url: getCurrentUrl(), format_id: "subs:" + hbarSubFmt + ":" + btn.dataset.sub });
      });
    });
    placeFmtsBelow();
  }

  function bindBarVideos() {
    for (const v of document.querySelectorAll("video")) {
      if (boundBarVideos.has(v)) continue;
      boundBarVideos.add(v);
      v.addEventListener("pointerenter", () => hbarShow(v));
      v.addEventListener("pointerleave", () => { if (!fmtsOpen) hbarHide(350); });
      v.addEventListener("pointermove", () => {
        if (hbarTarget === v) placeHbar();
      });
    }
    if (hbarTarget && !document.contains(hbarTarget)) hbarHide(0);
  }

  window.addEventListener("scroll", () => { if (hbarTarget) placeHbar(); }, true);
  window.addEventListener("resize", () => { if (hbarTarget) placeHbar(); });

  // YouTube SPA navigation (watch→watch, Shorts scroll): no reload happens,
  // so drop cached hover state + re-detect on the new video immediately.
  // getCurrentUrl() is already live, but the bar target / media registry /
  // open dropdown belong to the previous video.
  function resetForNav() {
    try { hbarHide(0); } catch (e) {}
    hbarTarget = null;
    pendingStream = null;
    try { if (hbarFmts) hbarFmts.classList.add("vx-hide"); } catch (e) {}
    fmtsOpen = false;
    media.clear();
    try { scan(); } catch (e) {}
    try { refreshUI(); } catch (e) {}
  }
  window.addEventListener("yt-navigate-finish", resetForNav);
  document.addEventListener("yt-navigate-finish", resetForNav);
  window.addEventListener("yt-page-data-updated", resetForNav);

  // ---------------- runtime hooks ----------------

  browser.runtime.onMessage.addListener((msg) => {
    if (msg && msg.type === "file_captured" && msg.capture) showToast(msg.capture);
    if (msg && msg.type === "stream_detected" && msg.url) {
      pendingStream = { url: msg.url, kind: msg.kind || "hls", pageUrl: msg.pageUrl || "", at: Date.now() };
      media.set(msg.url, { url: msg.url, title: document.title || "Stream video", kind: "hls", size: 0 });
      const v = document.querySelector("video");
      if (v) hbarShow(v);
      refreshUI();
    }
  });

  document.addEventListener("loadedmetadata", (e) => {
    if (e.target && (e.target.tagName === "VIDEO" || e.target.tagName === "AUDIO")) {
      scan();
      refreshUI();
    }
  }, true);

  const mo = new MutationObserver(() => {
    if (document.querySelectorAll("video, audio").length) {
      scan();
      refreshUI();
    }
  });
  mo.observe(document.documentElement, { childList: true, subtree: true });

  setInterval(() => {
    scan();
    if (panelOpen) renderPanel();
  }, 1800);

  window.addEventListener("beforeunload", () => {
    try { mo.disconnect(); } catch (e) { /* ignore */ }
  });
})();