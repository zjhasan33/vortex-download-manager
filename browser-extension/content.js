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

  const pageUrl = location.href;
  const pageTitle = document.title || "";

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
  let hbarFmts = null; // formats dropdown
  let fmtsOpen = false;
  const boundBarVideos = new WeakSet();

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
      media.get(url).title = String(pageTitle || fileName(url));
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
          register(v, pageUrl, "page");
        }
      } else {
        for (const s of v.querySelectorAll("source[src]")) {
          const e = extOf(s.src);
          if (MEDIA_EXT.includes(e)) register(v, s.src, e === "m3u8" ? "hls" : "media");
        }
      }
    }
    // youtube-like: video uses blob: — mark page-level
    if (onlyHost(hostOf(pageUrl)) && document.querySelector("video")) {
      if (!media.has("__page__")) media.set("__page__", { url: pageUrl, title: "This page's video", kind: "page" });
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
    const host = hostOf(pageUrl);
    const isVa = onlyHost(host);

    panel.querySelector(".vx-title").textContent = (pageTitle || host).slice(0, 60);

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
        const url = pageUrl;
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
          const subs = (res.info.subtitles || []).slice(0, 15);
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
        } else if (res && res.info && (res.info.subtitles || []).length) {
          let html2 = "";
          for (const s of res.info.subtitles.slice(0, 15)) {
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
    return onlyHost(hostOf(pageUrl));
  }

  function ensureHbarEls() {
    if (hbar) return;
    hbar = el("div", "vx-hbar vx-hide");
    hbar.addEventListener("pointerdown", (e) => e.stopPropagation());
    hbar.addEventListener("pointerenter", () => clearTimeout(hbarHideT));
    hbar.addEventListener("pointerleave", () => { if (!fmtsOpen) hbarHide(350); });
    document.documentElement.appendChild(hbar);
    hbarFmts = el("div", "vx-hb-fmts vx-hide");
    hbarFmts.addEventListener("pointerenter", () => { clearTimeout(hbarHideT); fmtsOpen = true; });
    hbarFmts.addEventListener("pointerleave", () => hbarHide(350));
    hbarFmts.addEventListener("pointerdown", (e) => e.stopPropagation());
    document.documentElement.appendChild(hbarFmts);
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
    hbar.innerHTML = '<span class="vx-logo"></span><span class="vx-hb-label">Download:</span>';
    hbar.appendChild(makeHbarBtn("MP4 Best", () => hbarStart("bestvideo+bestaudio/best", "MP4 Best")));
    hbar.appendChild(makeHbarBtn("720p", () => hbarStart("bestvideo[height<=720]+bestaudio/best[height<=720]", "720p")));
    hbar.appendChild(makeHbarBtn("MP3", () => hbarStart("ba-mp3-320", "MP3")));
    hbar.appendChild(makeHbarBtn("Subs", () => hbarStart("subs:srt:en", "Subs")));
    hbar.appendChild(el("span", "vx-hb-sep"));
    hbar.appendChild(makeHbarBtn("▾ Formats", () => hbarFormats()));
  }

  function barForDirect(m) {
    hbar.innerHTML = '<span class="vx-logo"></span><span class="vx-hb-label">Vortex</span>';
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
    const r = hbarTarget.getBoundingClientRect();
    if (r.width < 60 || r.height < 30 || r.bottom < 0 || r.top > window.innerHeight) return;
    const w = hbar.offsetWidth || 150;
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

  function hbarShow(video) {
    clearTimeout(hbarHideT);
    if (!video || !document.contains(video)) return;
    const direct = findMediaForVideo(video);
    if (!mediaSite() && !direct) return;
    ensureHbarEls();
    hbarTarget = video;
    if (mediaSite()) barForMediaSite();
    else barForDirect(direct);
    hbar.classList.remove("vx-hide");
    placeHbar();
  }

  function hbarHide(ms) {
    clearTimeout(hbarHideT);
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

  async function hbarStart(formatId, label2) {
    if (hbarFmts) hbarFmts.classList.add("vx-hide");
    const btn = [...hbar.querySelectorAll(".vx-hb-btn")].find((x) => x.textContent.trim() === label2);
    if (!btn) return;
    btn.disabled = true;
    btn.textContent = "…";
    const res = await send({ type: "start_ytdl", url: pageUrl, format_id: formatId });
    if (res && res.ok) {
      btn.classList.add("vx-hb-added");
      btn.textContent = "Added ✓";
    } else {
      btn.textContent = "Vortex off?";
    }
    setTimeout(() => {
      btn.classList.remove("vx-hb-added");
      btn.textContent = label2;
      btn.disabled = false;
    }, 2500);
  }

  async function hbarFormats() {
    fmtsOpen = true;
    hbarFmts.classList.remove("vx-hide");
    hbarFmts.innerHTML = '<div class="vx-empty" style="padding:10px"><span class="vx-spin"></span> Fetching formats…</div>';
    placeFmtsBelow();
    const u = pageUrl;
    const res = await send({ type: "analyze", url: u });
    const subs = (res && res.info && res.info.subtitles || []).slice(0, 15);
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
        void send({ type: "start_ytdl", url: u, format_id: btn.dataset.fid });
      });
    });
    hbarFmts.querySelectorAll("[data-sub]").forEach((btn) => {
      btn.addEventListener("click", () => {
        btn.textContent = "Added ✓";
        btn.disabled = true;
        void send({ type: "start_ytdl", url: u, format_id: "subs:srt:" + btn.dataset.sub });
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

  // ---------------- runtime hooks ----------------

  browser.runtime.onMessage.addListener((msg) => {
    if (msg && msg.type === "file_captured" && msg.capture) showToast(msg.capture);
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