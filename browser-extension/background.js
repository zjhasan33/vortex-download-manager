// Vortex Companion — background service worker.
// Bridges the page/content-script with the desktop Vortex app over a local WebSocket.
// Also acts like an IDM extension: watches network traffic, grabs media/file URLs,
// context menus, and notifications.

const WS_ADDR = "ws://127.0.0.1:17190";
const MEDIA_EXT = ["mp4", "webm", "mov", "m4v", "mkv", "flv", "avi", "m4a", "mp3", "ogg", "oga", "opus", "wav", "aac", "flac", "m3u8", "mpd"];
const FILE_EXT = ["zip", "rar", "7z", "tar", "gz", "bz2", "xz", "iso", "exe", "msi", "pdf", "apk", "deb", "rpm", "dmg", "torrent", "bin", "img", "whl", "epub", "doc", "docx", "xls", "xlsx", "ppt", "pptx"];
// Telemetry / page assets / text / images / code — never considered downloadable.
const NOISE_EXT = ["txt", "log", "json", "jsonp", "js", "mjs", "cjs", "jsx", "ts", "tsx", "css", "scss", "less", "map", "html", "htm", "xhtml", "xml", "csv", "md", "yml", "yaml", "svg", "ico", "png", "jpg", "jpeg", "gif", "webp", "avif", "bmp", "woff", "woff2", "ttf", "otf", "eot", "php", "asp", "aspx", "jsp", "vue", "crt", "pem", "ini", "conf", "wasm", "dll"];
const MEDIA_SITES = ["youtube.com", "youtu.be", "youtube-nocookie.com", "tiktok.com", "instagram.com", "facebook.com", "fb.watch", "twitter.com", "x.com", "dailymotion.com", "vimeo.com", "soundcloud.com", "bilibili.com", "twitch.tv", "reddit.com"];
const CAPTURES_KEY = "vx_captures";
const NOTIFY_KEY = "vx_notify";

// User preference: show desktop/browser notification on detected link.
let notifyEnabled = true;
chrome.storage.local.get({ [NOTIFY_KEY]: true }).then((r) => {
  notifyEnabled = r[NOTIFY_KEY] !== false;
});
chrome.storage.onChanged.addListener((changes, area) => {
  if (area === "local" && changes[NOTIFY_KEY]) {
    notifyEnabled = changes[NOTIFY_KEY].newValue !== false;
  }
});

let ws = null;
let wsReady = null; // promise controlling the current connection
let online = false;
let wsSerial = 0;
let autoAuthed = false;
const pending = new Map(); // req -> {resolve, reject, timer}

const tabTitles = new Map();
chrome.tabs.onUpdated.addListener((tabId, changeInfo, tab) => {
  if (changeInfo.title) tabTitles.set(tabId, changeInfo.title);
  if (changeInfo.status === "loading" && tab.url) tabTitles.set(tabId, tab.title || "");
});

// ---------------- WebSocket core ----------------

function connect() {
  if (wsReady) return wsReady;
  wsReady = connectAsync().catch((e) => {
    wsReady = null;
    throw e;
  });
  wsReady.catch(() => {});
  return wsReady;
}

async function connectAsync() {
  const sock = new WebSocket(WS_ADDR);
  ws = sock;
  await new Promise((resolve, reject) => {
    sock.onopen = () => resolve();
    sock.onerror = reject;
    sock.onclose = () => reject(new Error("closed"));
  });

  // ---- Authenticate with the desktop token (pairing key) ----
  try {
    const token = (await chrome.storage.local.get({ vx_token: "" })).vx_token;
    if (!token) throw new Error("Not paired yet: open the Vortex extension popup and paste the connection key");
    const authResp = await new Promise((resolve) => {
      const t = setTimeout(() => resolve(null), 5000);
      sock.onmessage = (ev) => {
        try {
          const m = JSON.parse(ev.data);
          if (m.type === "auth") {
            clearTimeout(t);
            resolve(m);
          }
        } catch (e) { /* ignore */ }
      };
      sock.send(JSON.stringify({ type: "auth", token }));
    });
    if (!authResp || !authResp.ok) throw new Error("desktop auth failed");
  } catch (e) {
    sock.close();
    throw e;
  }

  online = true;
  sock.onmessage = (ev) => {
    try {
      const msg = JSON.parse(ev.data);
      if (msg.req && pending.has(msg.req)) {
        const p = pending.get(msg.req);
        pending.delete(msg.req);
        clearTimeout(p.timer);
        if (p.rejectTimer) clearTimeout(p.rejectTimer);
        if (msg.type === "error") p.resolve({ error: msg.error });
        else p.resolve(msg);
      }
    } catch (e) { /* ignore */ }
  };
  const fail = () => {
    online = false;
    ws = null;
    wsReady = null;
  };
  sock.onerror = fail;
  sock.onclose = fail;
  return true;
}

function rpc(type, payload, timeoutMs = 60000) {
  return connect().then(() => {
    return new Promise((resolve, reject) => {
      const req = "x" + (++wsSerial) + "_" + Date.now();
      const timer = setTimeout(() => {
        pending.delete(req);
        reject(new Error("timeout: " + type));
      }, timeoutMs);
      pending.set(req, { resolve, reject, timer, rejectTimer: null });
      ws.send(JSON.stringify({ type, payload, req }));
    });
  }).catch((e) => ({ error: (e && e.message) || "Vortex is not running" }));
}

function isOnline() {
  return online;
}

function launchVortex(cmd, params) {
  // cmd: "capture"; params: object of url/filename[... ] -> percent-encoded query
  const q = new URLSearchParams(params).toString();
  return chrome.tabs.create({ url: "vortex://" + cmd + (q ? "?" + q : ""), active: false }).then(() => true);
}

// ---------------- media / filename helpers ----------------

function fileNameFromUrl(url) {
  try {
    const u = new URL(url);
    let name = decodeURIComponent(u.pathname.split("/").pop() || "");
    if (!name || name === "/") name = u.hostname;
    return name;
  } catch (e) {
    return url.split("/").pop() || url;
  }
}

function hostOf(url) {
  try { return new URL(url).hostname; } catch (e) { return ""; }
}

function isMediaSite(url) {
  const h = hostOf(url).toLowerCase();
  return MEDIA_SITES.some((s) => h === s || h.endsWith("." + s));
}

function extOf(url) {
  try {
    const path = new URL(url).pathname.toLowerCase();
    const m = path.match(/\.([a-z0-9]{2,5})$/);
    return m ? m[1] : "";
  } catch (e) { return ""; }
}

// ---------------- network capture (IDM-like) ----------------

function addCapture(cap) {
  chrome.storage.local.get({ [CAPTURES_KEY]: [] }).then((r) => {
    let list = r[CAPTURES_KEY] || [];
    const dup = list.find((c) => c.url === cap.url);
    if (dup) list = list.map((c) => (c.url === cap.url ? { ...cap, at: Date.now() } : c));
    else {
      list.unshift(cap);
      list = list.slice(0, 50);
    }
    chrome.storage.local.set({ [CAPTURES_KEY]: list });
    const last = list[0];
    chrome.runtime.sendMessage({ type: "file_captured", capture: last }).catch(() => {});
    lastHud = last;
  });
}
let lastHud = null;
const notifiedAt = new Map(); // key ("u:<url>" or "h:<host>") -> timestamp

function maybeNotify(cap) {
  // 1) Respect the user's notification toggle.
  if (!notifyEnabled) return;

  const now = Date.now();
  const host = hostOf(cap.url);
  const urlKey = "u:" + cap.url;
  const hostKey = "h:" + host;

  // 2) Debounce: no more than one notification per URL / per domain in 30s.
  const DEBOUNCE = 30000;
  if (now - (notifiedAt.get(urlKey) || 0) < DEBOUNCE) return;
  if (host && now - (notifiedAt.get(hostKey) || 0) < DEBOUNCE) return;

  notifiedAt.set(urlKey, now);
  if (host) notifiedAt.set(hostKey, now);
  if (notifiedAt.size > 500) {
    for (const [k, t] of notifiedAt) if (now - t > 60000) notifiedAt.delete(k);
  }

  try {
    chrome.notifications.create({
      type: "basic",
      iconUrl: "icons/icon128.png",
      title: "Vortex — link detected",
      message: (cap.filename || fileNameFromUrl(cap.url)) + " — click the Vortex button to download",
      priority: 1,
      buttons: [{ title: "Download with Vortex" }],
    });
  } catch (e) { /* no notifications permission */ }
}

chrome.notifications.onButtonClicked.addListener((notifId, btnIdx) => {
  chrome.notifications.clear(notifId);
  if (btnIdx === 0 && lastHud) {
    handleStart({ type: "direct", url: lastHud.url, filename: lastHud.filename });
  }
});

function looksDownloadable(url, headers) {
  const ext = extOf(url);
  // Never capture telemetry / text / code / page assets / images.
  if (NOISE_EXT.includes(ext)) return false;
  const cd = (headers["content-disposition"] || "").toLowerCase();
  const ct = (headers["content-type"] || "").toLowerCase();
  // Fast accept for real media and download archives.
  if (MEDIA_EXT.includes(ext) || FILE_EXT.includes(ext)) return true;
  // Explicit attachment downloads (2xx) are accepted for any non-noise type.
  if (cd.includes("attachment")) return true;
  if (cd.startsWith("inline")) return false;
  // Binary-ish application types that are typically large files.
  if (ct.startsWith("application/octet-stream") || ct.includes("zip") || ct.includes("pdf") || ct.includes("msword")) return true;
  return false;
}

function filenameFromDisposition(cd) {
  if (!cd) return null;
  const utf = cd.match(/filename\*\s*=\s*(?:UTF-8''|utf-8'')([^;\s]+)/i);
  if (utf) {
    try {
      return decodeURIComponent(utf[1]);
    } catch (e) { /* fall through */ }
  }
  const m = cd.match(/filename="?([^";]+)"?/i);
  return m ? m[1].trim().replace(/^["']|["']$/g, "") : null;
}

let capturedUrls = new Set();

chrome.webRequest.onBeforeRequest.addListener(
  (details) => {
    if (!(details.type === "media" || details.type === "object")) return;
    const url = details.url;
    if (!url.startsWith("http")) return;
    if (capturedUrls.has(url)) return;
    const ext = extOf(url);
    if (!MEDIA_EXT.includes(ext) && !url.includes("mime=")) return;
    capturedUrls.add(url);
    const tab = details.tabId > 0 ? details.tabId : undefined;
    const pageUrl = details.documentUrl || (tab ? "" : "");
    addCapture({
      url,
      filename: fileNameFromUrl(url),
      kind: ext === "m3u8" ? "hls" : "media",
      pageUrl,
      tabId: tab,
      viaHtml: false,
      at: Date.now(),
    });
    if (tab) maybeNotify({ url, filename: fileNameFromUrl(url) });
  },
  { urls: ["<all_urls>"], types: ["media", "object"] },
  []
);

chrome.webRequest.onHeadersReceived.addListener(
  (details) => {
    if (details.url.startsWith("data:") || details.url.startsWith("blob:")) return;
    const headers = {};
    for (const h of details.responseHeaders || []) headers[h.name.toLowerCase()] = h.value || "";
    if (!looksDownloadable(details.url, headers)) return;
    if (capturedUrls.has(details.url)) return;
    capturedUrls.add(details.url);
    const cdFilename = filenameFromDisposition(headers["content-disposition"]);
    let size = 0;
    const cl = headers["content-length"];
    if (cl) size = parseInt(cl, 10) || 0;
    // skip tiny inline responses unless it's an actual attachment
    if (!headers["content-disposition"] && size > 0 && size < 1024 * 1024 && !FILE_EXT.includes(extOf(details.url))) return;

    const tabId = details.tabId > 0 ? details.tabId : undefined;
    addCapture({
      url: details.url,
      filename: cdFilename || fileNameFromUrl(details.url),
      size,
      kind: "file",
      pageUrl: tabId ? details.documentUrl || "" : "",
      tabId,
      viaHtml: false,
      at: Date.now(),
    });
    if (tabId) maybeNotify({ url: details.url, filename: cdFilename || fileNameFromUrl(details.url) });
  },
  { urls: ["<all_urls>"], types: ["main_frame", "sub_frame", "other", "xmlhttprequest", "object"] },
  ["responseHeaders", "extraHeaders"]
);

setInterval(() => {
  capturedUrls = new Set([...capturedUrls].slice(-300));
}, 10 * 60 * 1000);

// keep SW alive-ish: reconnect attempt every 15s when offline
setInterval(() => {
  if (!online) connect().catch(() => {});
}, 15000);

// ---------------- context menus ----------------

chrome.runtime.onInstalled.addListener(() => {
  chrome.contextMenus.removeAll(() => {
    chrome.contextMenus.create({
      id: "vx-download-link",
      title: "Download link with Vortex",
      contexts: ["link"],
    });
    chrome.contextMenus.create({
      id: "vx-download-media",
      title: "Download this video/audio with Vortex",
      contexts: ["video", "audio"],
    });
    chrome.contextMenus.create({
      id: "vx-download-page",
      title: "Download this page's video with Vortex",
      contexts: ["page"],
    });
    chrome.contextMenus.create({
      id: "vx-grab-all",
      title: "Grab all links on this page with Vortex",
      contexts: ["page"],
    });
  });
});

chrome.contextMenus.onClicked.addListener((info, tab) => {
  if (info.menuItemId === "vx-download-link" && info.linkUrl) {
    if (isMediaSite(info.linkUrl)) handleAnalyze(info.linkUrl, tab);
    else handleStart({ type: "direct", url: info.linkUrl });
  } else if (info.menuItemId === "vx-download-media" && info.srcUrl) {
    if (info.srcUrl.startsWith("blob:")) {
      if (tab && tab.url && tab.url.startsWith("http")) handleAnalyze(tab.url, tab);
    } else handleStart({ type: "direct", url: info.srcUrl });
  } else if (info.menuItemId === "vx-download-page" && tab && tab.url) {
    handleAnalyze(tab.url, tab);
  } else if (info.menuItemId === "vx-grab-all" && tab && tab.id > 0) {
    handleGrabAll(tab);
  }
});

// Collect every http(s) link on the page, then queue them all in Vortex.
function collectPageLinks() {
  const out = [];
  const seen = new Set();
  document.querySelectorAll("a[href]").forEach((a) => {
    try {
      const u = new URL(a.href, document.baseURI);
      if (u.protocol !== "http:" && u.protocol !== "https:") return;
      u.hash = "";
      const href = u.toString();
      if (seen.has(href)) return;
      seen.add(href);
      const text = (a.textContent || "").trim().slice(0, 80);
      out.push({ url: href, text });
    } catch (e) { /* skip bad hrefs */ }
  });
  return out;
}

async function handleGrabAll(tab) {
  let links = [];
  try {
    const res = await chrome.scripting.executeScript({
      target: { tabId: tab.id },
      func: collectPageLinks,
    });
    links = (res && res[0] && res[0].result) || [];
  } catch (e) {
    chrome.notifications.create({
      type: "basic",
      iconUrl: "icons/icon128.png",
      title: "Vortex — grab all links",
      message: "Cannot read links on this page: " + (e && e.message ? e.message : e),
      priority: 1,
    });
    return;
  }
  links = links.slice(0, 100);
  if (!links.length) {
    chrome.notifications.create({
      type: "basic",
      iconUrl: "icons/icon128.png",
      title: "Vortex — grab all links",
      message: "No links found on this page",
      priority: 1,
    });
    return;
  }
  let queued = 0;
  for (const l of links) {
    const r = await handleStart({ type: "direct", url: l.url, filename: fileNameFromUrl(l.url) });
    if (r && !r.error) queued++;
  }
  chrome.notifications.create({
    type: "basic",
    iconUrl: "icons/icon128.png",
    title: "Vortex — grab all links",
    message: queued + " of " + links.length + " link(s) queued in Vortex",
    priority: 1,
  });
}

// ---------------- action dispatcher (used by content + popup + menus) ----------------

async function handleStart({ type, url, filename, pageUrl }) {
  if (!url) return { error: "no url" };
  if (type === "direct" || isMediaSite(url) || type === "auto") {
    const isYT = isMediaSite(url);
    const isHLS = extOf(url) === "m3u8" || url.includes(".m3u8");
    const payload = { url, filename: filename || undefined };
    if (isYT || isHLS) payload.via = "yt";
    const res = await rpc("download", payload);
    if (online) {
      // The desktop acks immediately with { success:true, action:... }.
      if (res && res.error) return res;
      return { success: true, ok: true, action: (res && res.action) || "download_started" };
    }
    if (isYT || isHLS) {
      await launchVortex("capture", { url, via: "yt", filename: filename || "" });
      return { ok: true, launched: true };
    }
    if (filename) await launchVortex("capture", { url, filename });
    else await launchVortex("capture", { url });
    return { ok: true, launched: true, fromCaptures: true };
  }
  return { error: "unsupported" };
}

async function handleAnalyze(url, tab) {
  const res = await rpc("analyze", { url });
  if (online) return res;
  const pageUrl = tab && tab.url ? tab.url : url;
  const title = tabTitles.get(tab && tab.id) || "";
  await launchVortex("capture", { url: pageUrl, via: "yt" });
  return { ok: false, launched: true, analyzeUrl: url, title, error: res.error };
}

// ---------------- message router ----------------

// Always answer the sender exactly once, even if the handler throws or hangs.
// Returns true so Chrome keeps the response port open for async work.
function safeRespond(sendResponse, work, fallbackMs = 10000) {
  let done = false;
  const finish = (res) => {
    if (done) return;
    done = true;
    try {
      sendResponse(res || { error: "no response" });
    } catch (e) {
      /* port already closed */
    }
  };
  const timer = setTimeout(() => finish({ error: "Vortex bridge timeout" }), fallbackMs);
  Promise.resolve()
    .then(work)
    .then((res) => {
      clearTimeout(timer);
      finish(res);
    })
    .catch((e) => {
      clearTimeout(timer);
      finish({ error: (e && e.message) || String(e) });
    });
  return true;
}

chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  const tab = sender.tab;
  const pageUrl = (tab && tab.url) || "";
  const title = (tab && tab.title) || tabTitles.get(tab && tab.id) || "";

  if (msg.type === "ping") {
    sendResponse({ ok: isOnline(), running: online });
    return; // synchronous response
  }

  if (msg.type === "download_media") {
    const url = msg.url || "";
    return safeRespond(sendResponse, async () => {
      if (url.startsWith("blob:") || isMediaSite(pageUrl)) {
        if (online) return { ...(await handleAnalyze(pageUrl)), action: "analyze" };
        await launchVortex("capture", { url: pageUrl, via: "yt" });
        return { ok: false, action: "analyze", launched: true, title };
      }
      return handleStart({ type: "direct", url, filename: msg.filename, pageUrl });
    });
  }

  if (msg.type === "download_direct") {
    if (!msg.url) {
      sendResponse({ error: "no url" });
      return;
    }
    return safeRespond(sendResponse, () =>
      handleStart({ type: "direct", url: msg.url, filename: msg.filename, pageUrl })
    );
  }

  if (msg.type === "analyze") {
    return safeRespond(sendResponse, () => handleAnalyze(msg.url, sender.tab));
  }

  if (msg.type === "start_ytdl") {
    if (online) return safeRespond(sendResponse, () => rpc("start_ytdl", { url: msg.url, format_id: msg.format_id }));
    return safeRespond(sendResponse, async () => {
      await launchVortex("capture", { url: msg.url, via: "yt", format: msg.format_id });
      return { ok: false, launched: true };
    });
  }

  if (msg.type === "open_vortex") {
    return safeRespond(sendResponse, async () => ({ ok: await launchVortex("open", {}) }));
  }

  if (msg.type === "get_stats") {
    return safeRespond(sendResponse, () => rpc("stats", null, 8000), 9000);
  }

  if (msg.type === "list_captures") {
    return safeRespond(sendResponse, async () => {
      const r = await chrome.storage.local.get({ [CAPTURES_KEY]: [] });
      return { captures: r[CAPTURES_KEY] || [], online };
    });
  }

  if (msg.type === "clear_captures") {
    return safeRespond(sendResponse, async () => {
      await chrome.storage.local.set({ [CAPTURES_KEY]: [] });
      return { ok: true };
    });
  }

  // Unknown message type — never leave the caller hanging.
  sendResponse({ error: "unknown message: " + msg.type });
});

// wake the worker on tab navigation to keep WS fresh
chrome.tabs.onUpdated.addListener((tabId) => {
  void tabId;
  if (!online) connect().catch(() => {});
});