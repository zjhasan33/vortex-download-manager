
// Vortex Companion — Universal Responsive Video Hover Bar (All-Sites + Fullscreen)
(function () {
  if (window.__vortexContentInstalled) return;
  window.__vortexContentInstalled = true;

  if (typeof browser === "undefined" && typeof globalThis.chrome !== "undefined") {
    var browser = globalThis.chrome;
  }

  const MEDIA_EXT = ["mp4", "webm", "mov", "m4v", "mkv", "flv", "m4a", "mp3", "ogg", "oga", "opus", "wav", "aac", "flac", "m3u8", "mpd"];

  function getCurrentUrl() {
    try {
      const path = location.pathname || "";
      const pm = path.match(/^\/shorts\/([\w-]{6,})/);
      if (pm) return "https://www.youtube.com/shorts/" + pm[1];
    } catch (e) {}
    return location.href;
  }

  function isYouTube() {
    const h = location.hostname.toLowerCase();
    return h.includes("youtube.com") || h.includes("youtu.be");
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

  function el(tag, cls, text) {
    const n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text != null) n.textContent = text;
    return n;
  }

  function send(msg) {
    try {
      return browser.runtime.sendMessage(msg).then(
        (res) => res || {},
        (e) => ({ error: (e && e.message) || String(e) })
      );
    } catch (e) {
      return Promise.resolve({ error: String(e) });
    }
  }

  // ---------------- State ----------------
  const media = new Map();
  let hbar = null;
  let hbarTarget = null;
  let hbarHideT = 0;
  let hbarFmts = null;
  let fmtsOpen = false;
  let hbarSubFmt = "srt";
  let isDragging = false;
  let dragOffsetX = 0;
  let dragOffsetY = 0;
  let customPos = null;
  let pendingStream = null;

  browser.storage.local.get({ vx_hbar_pos: null }).then((r) => {
    if (r.vx_hbar_pos && typeof r.vx_hbar_pos.rightOffset === "number") {
      customPos = r.vx_hbar_pos;
    }
  });

  function getBestThumbnail() {
    try {
      const meta = document.querySelector('meta[property="og:image"]');
      if (meta && meta.content) return meta.content;
      const v = document.querySelector("video");
      if (v && v.poster) return v.poster;
    } catch (e) {}
    return "";
  }

  // Universal Player Container Resolver (YouTube, Vimeo, HTML5, VideoJS, etc.)
  function getPlayerContainer(target) {
    if (!target) return null;
    const player = target.closest(
      ".html5-video-player, #movie_player, .video-js, .vjs-tech, [data-player], .player-container, .jwplayer, .plyr, .dplayer"
    );
    if (player) return player;
    let p = target.parentElement;
    if (p && p !== document.body && p !== document.documentElement) {
      const r = p.getBoundingClientRect();
      if (r.width >= 160 && r.height >= 90) return p;
    }
    return target;
  }

  // ---------------- UI Setup ----------------
  function ensureHbarEls() {
    if (hbar) return;
    hbar = el("div", "vx-hbar vx-hide");
    hbar.style.position = "fixed";
    hbar.style.zIndex = "2147483647";
    hbar.addEventListener("pointerdown", (e) => e.stopPropagation());
    hbar.addEventListener("mouseenter", () => clearTimeout(hbarHideT));
    hbar.addEventListener("mouseleave", () => { if (!fmtsOpen && !isDragging) hbarHide(400); });
    document.documentElement.appendChild(hbar);

    // Draggable Grip
    hbar.addEventListener("mousedown", (e) => {
      if (e.button !== 0 || isDragging) return;
      if (e.target.closest("button, a, input, select")) return;
      e.preventDefault();
      e.stopPropagation();
      e.stopImmediatePropagation();
      const r = hbar.getBoundingClientRect();
      dragOffsetX = e.clientX - r.left;
      dragOffsetY = e.clientY - r.top;
      isDragging = true;
      hbar.classList.add("vx-hb-dragging");
    });

    // Double-click grip resets to default Top-Right corner!
    hbar.addEventListener("dblclick", (e) => {
      if (e.target.classList.contains("vx-hb-grip")) {
        customPos = null;
        void browser.storage.local.remove("vx_hbar_pos");
        placeHbar();
      }
    });

    window.addEventListener("mousemove", (e) => {
      if (!isDragging) return;
      e.preventDefault();
      const nx = Math.max(2, Math.min(window.innerWidth - (hbar.offsetWidth || 180) - 2, e.clientX - dragOffsetX));
      const ny = Math.max(2, Math.min(window.innerHeight - (hbar.offsetHeight || 40) - 2, e.clientY - dragOffsetY));
      hbar.style.left = nx + "px";
      hbar.style.top = ny + "px";
    }, true);

    window.addEventListener("mouseup", () => {
      if (!isDragging) return;
      isDragging = false;
      hbar.classList.remove("vx-hb-dragging");
      const rBar = hbar.getBoundingClientRect();
      const player = getPlayerContainer(hbarTarget);
      const rPlayer = player ? player.getBoundingClientRect() : { right: window.innerWidth, top: 0 };
      customPos = {
        rightOffset: Math.max(0, Math.round(rPlayer.right - rBar.right)),
        topOffset: Math.max(0, Math.round(rBar.top - rPlayer.top))
      };
      void browser.storage.local.set({ vx_hbar_pos: customPos });
    }, true);

    hbarFmts = el("div", "vx-hb-fmts vx-hide");
    hbarFmts.style.position = "fixed";
    hbarFmts.style.zIndex = "2147483647";
    hbarFmts.style.maxHeight = "380px";
    hbarFmts.style.overflowY = "auto";
    hbarFmts.addEventListener("mouseenter", () => { clearTimeout(hbarHideT); fmtsOpen = true; });
    hbarFmts.addEventListener("mouseleave", () => hbarHide(400));
    hbarFmts.addEventListener("pointerdown", (e) => e.stopPropagation());
    document.documentElement.appendChild(hbarFmts);

    document.addEventListener("pointerdown", (e) => {
      if (fmtsOpen && !hbarFmts.contains(e.target) && !hbar.contains(e.target)) hbarHide(0);
    });
    document.addEventListener("keydown", (e) => {
      if (e.key === "Escape" && fmtsOpen) hbarHide(0);
    });
  }

  function makeBtn(label, onClick) {
    const b = el("button", "vx-hb-btn", label);
    b.addEventListener("click", onClick);
    return b;
  }

  function buildHbarContent() {
    hbar.innerHTML = '<span class="vx-hb-grip" title="Drag to move (Double-click to reset)">⋮⋮</span><span class="vx-logo"></span><span class="vx-hb-label">Download:</span>';

    if (isYouTube()) {
      hbar.appendChild(makeBtn("MP4 Best", () => triggerDownload("bestvideo+bestaudio/best", "MP4 Best")));
    } else {
      hbar.appendChild(makeBtn("Download Video", () => triggerUniversalDownload()));
    }

    hbar.appendChild(el("span", "vx-hb-sep"));
    hbar.appendChild(makeBtn("▾ Formats", () => openFormatsDropdown()));
  }

  // ---------------- Positioning Engine ----------------
  function placeHbar() {
    if (!hbar || !hbarTarget) return;
    if (isDragging) return;

    const player = getPlayerContainer(hbarTarget);
    if (!player) return;
    const r = player.getBoundingClientRect();
    if (r.width < 60 || r.height < 40) return;

    const w = hbar.offsetWidth || 190;
    const h = hbar.offsetHeight || 38;
    const isFs = !!document.fullscreenElement;

    // Relative user dragged offset
    if (customPos && typeof customPos.rightOffset === "number") {
      let x = isFs ? (window.innerWidth - w - customPos.rightOffset) : (r.right - w - customPos.rightOffset);
      let y = isFs ? customPos.topOffset : (r.top + customPos.topOffset);
      x = Math.max(8, Math.min(window.innerWidth - w - 8, x));
      y = Math.max(8, Math.min(window.innerHeight - h - 8, y));
      hbar.style.left = x + "px";
      hbar.style.top = y + "px";
      return;
    }

    // Default: Top-Right corner inside the video player (14px inset)
    const x = isFs ? (window.innerWidth - w - 16) : Math.max(8, r.right - w - 16);
    const y = isFs ? 16 : Math.max(8, r.top + 16);
    hbar.style.left = x + "px";
    hbar.style.top = y + "px";
  }

  function placeDropdown() {
    if (!hbar || !hbarFmts) return;
    const r = hbar.getBoundingClientRect();
    const bw = hbarFmts.offsetWidth || 280;
    const bh = hbarFmts.offsetHeight || 260;
    let x = r.right - bw;
    if (x < 4) x = 4;
    let y = r.bottom + 6;
    if (y + bh > window.innerHeight - 8) {
      y = Math.max(8, r.top - bh - 6);
    }
    hbarFmts.style.left = x + "px";
    hbarFmts.style.top = y + "px";
  }

  function hbarShow(targetEl) {
    clearTimeout(hbarHideT);
    ensureHbarEls();
    hbarTarget = targetEl;
    buildHbarContent();
    hbar.classList.remove("vx-hide");
    placeHbar();
  }

  function hbarHide(ms) {
    clearTimeout(hbarHideT);
    if (isDragging) return;
    hbarHideT = setTimeout(() => {
      if (hbar) hbar.classList.add("vx-hide");
      if (hbarFmts) hbarFmts.classList.add("vx-hide");
      fmtsOpen = false;
    }, ms || 400);
  }

  // ---------------- Downloads ----------------
  async function triggerDownload(formatId, label) {
    if (hbarFmts) hbarFmts.classList.add("vx-hide");
    const btn = [...hbar.querySelectorAll(".vx-hb-btn")].find((x) => x.textContent.trim() === label);
    if (btn) { btn.disabled = true; btn.textContent = "…"; }

    const url = getCurrentUrl();
    const thumb = getBestThumbnail();
    const res = await send({ type: "start_ytdl", url, format_id: formatId, thumbnail: thumb });

    if (btn) {
      btn.textContent = res && res.ok ? "Added ✓" : "Vortex off?";
      btn.classList.toggle("vx-hb-added", !!(res && res.ok));
      setTimeout(() => {
        btn.textContent = label;
        btn.disabled = false;
        btn.classList.remove("vx-hb-added");
      }, 2500);
    }
  }

  // Universal Video Downloader for non-YouTube sites
  async function triggerUniversalDownload() {
    const btn = [...hbar.querySelectorAll(".vx-hb-btn")].find((x) => x.textContent.trim().includes("Download Video"));
    if (btn) { btn.disabled = true; btn.textContent = "…"; }

    let targetUrl = getCurrentUrl();
    if (pendingStream && pendingStream.url) {
      targetUrl = pendingStream.url;
    } else if (hbarTarget) {
      const v = hbarTarget.tagName === "VIDEO" ? hbarTarget : hbarTarget.querySelector("video");
      if (v && v.currentSrc && !v.currentSrc.startsWith("blob:")) {
        targetUrl = v.currentSrc;
      }
    }

    const thumb = getBestThumbnail();
    let res = null;
    if (targetUrl.includes(".m3u8") || targetUrl.includes(".mpd")) {
      res = await send({ type: "start_ytdl", url: targetUrl, format_id: "bestvideo+bestaudio/best", thumbnail: thumb });
    } else {
      res = await send({ type: "download_direct", url: targetUrl, filename: document.title || "video.mp4" });
    }

    if (btn) {
      const rerr = res && res.error ? String(res.error) : "";
      btn.textContent = res && (res.ok || res.launched) ? "Added ✓" : (/protect|drm|login|offline/i.test(rerr) ? "Protected ✕" : "Vortex off?");
      btn.classList.toggle("vx-hb-added", !!(res && (res.ok || res.launched)));
      setTimeout(() => {
        btn.textContent = "Download Video";
        btn.disabled = false;
        btn.classList.remove("vx-hb-added");
      }, 2500);
    }
  }

  // ---------------- DOM Parser ----------------
  function parseYtFromDOM() {
    try {
      for (const s of document.scripts) {
        const t = s.textContent || "";
        if (t.includes("ytInitialPlayerResponse")) {
          const m = t.match(/ytInitialPlayerResponse\s*=\s*(\{.+?\});/s) || t.match(/ytInitialPlayerResponse\s*=\s*(\{.+?\})(?:;|\n|$)/s);
          if (m) return JSON.parse(m[1]);
        }
      }
    } catch (e) {}
    return null;
  }

  // ---------------- Master Dropdown ----------------
  async function openFormatsDropdown() {
    fmtsOpen = true;
    hbarFmts.classList.remove("vx-hide");
    const u = getCurrentUrl();
    let lastFid = "";
    try {
      const lr = await browser.storage.local.get({ vx_last_fid: "" });
      lastFid = lr.vx_last_fid || "";
    } catch (e) {}

    let dur = 0;
    try {
      const v = document.querySelector("video");
      if (v && isFinite(v.duration)) dur = v.duration;
    } catch (e) {}

    let html = "";

    // 1. VIDEO FORMATS
    html += '<div class="vx-empty" style="text-align:left;padding:6px 2px 2px;font-weight:bold;color:#00e5ff">Video Formats</div>';
    if (isYouTube()) {
      const videoQualities = [
        ["2160p 4K", "bestvideo[height<=2160]+bestaudio/best"],
        ["1440p 2K", "bestvideo[height<=1440]+bestaudio/best"],
        ["1080p Full HD", "bestvideo[height<=1080]+bestaudio/best"],
        ["720p HD", "bestvideo[height<=720]+bestaudio/best"],
        ["480p", "bestvideo[height<=480]+bestaudio/best"],
        ["360p", "bestvideo[height<=360]+bestaudio/best"],
      ];
      for (const [label, fid] of videoQualities) {
        html +=
          '<div class="vx-fmt"><span class="vx-fq">' + esc(label) + (fid === lastFid ? " ★" : "") + '</span>' +
          '<button class="vx-btn vx-slim" data-fid="' + esc(fid) + '">Download</button></div>';
      }
    } else {
      html +=
        '<div class="vx-fmt"><span class="vx-fq">Best Quality (Auto)</span>' +
        '<button class="vx-btn vx-slim" data-direct="1">Download</button></div>';
    }

    // 2. AUDIO FORMATS
    html += '<div class="vx-empty" style="text-align:left;padding:8px 2px 2px;font-weight:bold;color:#00e5ff">Audio Formats</div>';
    const audios = [
      ["MP3 • 320 kbps (Best)", "ba-audio-mp3-320", 320000],
      ["MP3 • 192 kbps (Standard)", "ba-audio-mp3-192", 192000],
      ["MP3 • 128 kbps (Compact)", "ba-audio-mp3-128", 128000],
      ["M4A • 256 kbps (AAC)", "ba-audio-m4a-256", 256000],
      ["FLAC • Lossless", "ba-audio-flac-0", 0],
      ["WAV • Lossless", "ba-audio-wav-0", 0],
    ];

    for (const [label, fid, br] of audios) {
      const sizeStr = dur && br ? " • " + fmtBytes((dur * br) / 8) : "";
      html +=
        '<div class="vx-fmt"><span class="vx-fq">' + esc(label) + sizeStr + (fid === lastFid ? " ★" : "") + '</span>' +
        '<button class="vx-btn vx-slim" data-fid="' + esc(fid) + '">Download</button></div>';
    }

    // 3. SUBTITLES & CAPTIONS
    if (isYouTube()) {
      html += '<div class="vx-empty" style="text-align:left;padding:8px 2px 2px;font-weight:bold;color:#00e5ff">Subtitles / Captions</div>';
      html +=
        '<div class="vx-subfmt" style="padding:4px 0">' +
        '<span class="vx-subfmt-lbl">Format:</span>' +
        '<button class="vx-btn vx-slim ' + (hbarSubFmt === "srt" ? "vx-on" : "") + '" data-subfmt="srt">SRT</button>' +
        '<button class="vx-btn vx-slim ' + (hbarSubFmt === "vtt" ? "vx-on" : "") + '" data-subfmt="vtt">VTT</button>' +
        '</div>';

      let captionsList = [];
      const json = parseYtFromDOM();
      if (json && json.captions && json.captions.playerCaptionsTracklistRenderer) {
        const tracks = json.captions.playerCaptionsTracklistRenderer.captionTracks || [];
        for (const tr of tracks) {
          const lang = tr.languageCode || "";
          let label = tr.name?.simpleText || tr.name?.runs?.[0]?.text || lang;
          const auto = tr.vssId?.startsWith("a.") || tr.kind === "asr" || /auto/i.test(label);
          if (auto && !/\(auto\)/i.test(label)) label += " (auto)";
          captionsList.push({ lang, label, auto: !!auto });
        }
      }

      if (captionsList.length) {
        for (const s of captionsList.slice(0, 20)) {
          html +=
            '<div class="vx-fmt"><span class="vx-fq">' + esc(s.label) + '</span>' +
            '<button class="vx-btn vx-slim" data-sub="' + esc(s.lang) + '" data-auto="' + (s.auto ? "1" : "") + '">.' + hbarSubFmt + '</button></div>';
        }
      } else {
        html += '<div class="vx-empty" style="font-size:11px;padding:4px">No subtitles found on page.</div>';
      }
    }

    hbarFmts.innerHTML = html;
    const thumb = getBestThumbnail();

    hbarFmts.querySelectorAll("[data-fid]").forEach((btn) => {
      btn.addEventListener("click", () => {
        btn.textContent = "Added ✓";
        btn.disabled = true;
        try { browser.storage.local.set({ vx_last_fid: btn.dataset.fid }); } catch (e) {}
        void send({ type: "start_ytdl", url: u, format_id: btn.dataset.fid, thumbnail: thumb });
      });
    });

    hbarFmts.querySelectorAll("[data-direct]").forEach((btn) => {
      btn.addEventListener("click", () => {
        btn.textContent = "Added ✓";
        btn.disabled = true;
        triggerUniversalDownload();
      });
    });

    hbarFmts.querySelectorAll("[data-subfmt]").forEach((btn) => {
      btn.addEventListener("click", () => {
        hbarSubFmt = btn.dataset.subfmt;
        openFormatsDropdown();
      });
    });

    hbarFmts.querySelectorAll("[data-sub]").forEach((btn) => {
      btn.addEventListener("click", () => {
        btn.textContent = "Added ✓";
        btn.disabled = true;
        void send({ type: "start_ytdl", url: u, format_id: "subs:" + hbarSubFmt + ":" + btn.dataset.sub, auto_subs: btn.dataset.auto === "1" });
      });
    });

    placeDropdown();
  }

  // ---------------- Master Event Listeners ----------------
  const boundElements = new WeakSet();

  function scanAndBind() {
    const targets = [];
    document.querySelectorAll("video").forEach((v) => {
      targets.push(v);
      const player = getPlayerContainer(v);
      if (player) targets.push(player);
    });

    for (const el of targets) {
      if (boundElements.has(el)) continue;
      boundElements.add(el);

      el.addEventListener("mouseenter", () => {
        const player = getPlayerContainer(el);
        if (player) hbarShow(player);
      });

      el.addEventListener("mouseleave", () => {
        if (!fmtsOpen && !isDragging) hbarHide(400);
      });

      el.addEventListener("mousemove", () => {
        if (hbar && !hbar.classList.contains("vx-hide") && !isDragging) placeHbar();
      });
    }
  }

  // Fullscreen support: Move hover bar inside fullscreen element so it NEVER disappears!
  function handleFullscreen() {
    const fsEl = document.fullscreenElement || document.webkitFullscreenElement;
    if (fsEl && hbar) {
      fsEl.appendChild(hbar);
      if (hbarFmts) fsEl.appendChild(hbarFmts);
    } else if (hbar) {
      document.documentElement.appendChild(hbar);
      if (hbarFmts) document.documentElement.appendChild(hbarFmts);
    }
    setTimeout(placeHbar, 100);
  }
  document.addEventListener("fullscreenchange", handleFullscreen);
  document.addEventListener("webkitfullscreenchange", handleFullscreen);

  function onNav() {
    hbarHide(0);
    hbarTarget = null;
    fmtsOpen = false;
    setTimeout(scanAndBind, 500);
  }
  window.addEventListener("yt-navigate-finish", onNav);
  document.addEventListener("yt-navigate-finish", onNav);

  window.addEventListener("scroll", () => { if (hbarTarget && !isDragging) placeHbar(); }, true);
  window.addEventListener("resize", () => { if (hbarTarget && !isDragging) placeHbar(); });

  browser.runtime.onMessage.addListener((msg) => {
    if (msg && msg.type === "stream_detected" && msg.url) {
      pendingStream = { url: msg.url, kind: msg.kind || "hls", at: Date.now() };
      const v = document.querySelector("video");
      if (v) hbarShow(getPlayerContainer(v));
    }
  });

  const mo = new MutationObserver(() => scanAndBind());
  mo.observe(document.documentElement, { childList: true, subtree: true });

  setInterval(scanAndBind, 1500);
  scanAndBind();
})();