// Vortex Companion — background event page.
// Bridges the page/content-script with the desktop Vortex app over a local WebSocket.
// Also acts like an IDM extension: watches network traffic, grabs media/file URLs,
// context menus, and notifications.

// Dual-browser support: Firefox provides `browser.*` natively; on Chrome alias the
// same promise-based namespace so one codebase runs on both.
if (typeof browser === "undefined" && typeof globalThis.chrome !== "undefined") {
  var browser = globalThis.chrome;
}

const WS_ADDR = "ws://127.0.0.1:17190";
const MEDIA_EXT = ["mp4", "webm", "mov", "m4v", "mkv", "flv", "avi", "m4a", "mp3", "ogg", "oga", "opus", "wav", "aac", "flac", "m3u8", "mpd"];
const FILE_EXT = ["zip", "rar", "7z", "tar", "gz", "bz2", "xz", "iso", "exe", "msi", "pdf", "apk", "deb", "rpm", "dmg", "torrent", "bin", "img", "whl", "epub", "doc", "docx", "xls", "xlsx", "ppt", "pptx"];
// Telemetry / page assets / text / images / code — never considered downloadable.
const NOISE_EXT = ["txt", "log", "json", "jsonp", "js", "mjs", "cjs", "jsx", "ts", "tsx", "css", "scss", "less", "map", "html", "htm", "xhtml", "xml", "csv", "md", "yml", "yaml", "svg", "ico", "png", "jpg", "jpeg", "gif", "webp", "avif", "bmp", "woff", "woff2", "ttf", "otf", "eot", "php", "asp", "aspx", "jsp", "vue", "crt", "pem", "ini", "conf", "wasm", "dll"];
const MEDIA_SITES = ["youtube.com", "youtu.be", "youtube-nocookie.com", "tiktok.com", "instagram.com", "facebook.com", "fb.watch", "twitter.com", "x.com", "dailymotion.com", "vimeo.com", "soundcloud.com", "bilibili.com", "twitch.tv", "reddit.com"];
const CAPTURES_KEY = "vx_captures";
const NOTIFY_KEY = "vx_notify";

// User preference: show desktop/browser notification on detected link.
// DEFAULT: disabled — notifications for link detection are off unless the user opts in.
let notifyEnabled = false;
browser.storage.local.get({ [NOTIFY_KEY]: false }).then((r) => {
  notifyEnabled = r[NOTIFY_KEY] === true;
});
browser.storage.onChanged.addListener((changes, area) => {
  if (area === "local" && changes[NOTIFY_KEY]) {
    notifyEnabled = changes[NOTIFY_KEY].newValue === true;
  }
});

// User preference: embed subtitles into yt-dlp video downloads (IDM-like).
const SUB_EMBED_KEY = "vx_subs_embed";
let subsEnabled = true;
browser.storage.local.get({ [SUB_EMBED_KEY]: true }).then((r) => {
  subsEnabled = r[SUB_EMBED_KEY] !== false;
});
browser.storage.onChanged.addListener((changes, area) => {
  if (area === "local" && changes[SUB_EMBED_KEY]) {
    subsEnabled = changes[SUB_EMBED_KEY].newValue !== false;
  }
});

// User preference: capture browser-initiated downloads and hand them to Vortex
// (the IDM-style "default downloader" behaviour). DEFAULT: on.
const INTERCEPT_KEY = "vx_intercept";
let interceptEnabled = true;
browser.storage.local.get({ [INTERCEPT_KEY]: true }).then((r) => {
  interceptEnabled = r[INTERCEPT_KEY] !== false;
});
browser.storage.onChanged.addListener((changes, area) => {
  if (area === "local" && changes[INTERCEPT_KEY]) {
    interceptEnabled = changes[INTERCEPT_KEY].newValue !== false;
  }
});

let ws = null;
let wsReady = null; // promise controlling the current connection
let online = false;
let wsSerial = 0;
let autoAuthed = false;
const pending = new Map(); // req -> {resolve, reject, timer}

const tabTitles = new Map();
browser.tabs.onUpdated.addListener((tabId, changeInfo, tab) => {
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
    // Fail fast instead of hanging forever: a silent OS-level drop of the SYN
    // used to make rpc() never settle, which surfaced as
    // "No response from Vortex bridge (timed out)" in the popup.
    const t = setTimeout(() => reject(new Error("Vortex bridge not listening")), 4000);
    sock.onopen = () => {
      clearTimeout(t);
      resolve();
    };
    sock.onerror = () => {
      clearTimeout(t);
      reject(new Error("closed"));
    };
    sock.onclose = () => {
      clearTimeout(t);
      reject(new Error("closed"));
    };
  });

  // ---- Authenticate with the desktop token (pairing key) ----
  try {
    const token = (await browser.storage.local.get({ vx_token: "" })).vx_token;
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
  return browser.tabs.create({ url: "vortex://" + cmd + (q ? "?" + q : ""), active: false }).then(() => true);
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
    // Strip query/hash first so "file.txt?key=123" can't bypass extension filters.
    const clean = String(url).split("?")[0].split("#")[0];
    const path = new URL(clean).pathname.toLowerCase();
    const m = path.match(/\.([a-z0-9]{2,5})$/);
    return m ? m[1] : "";
  } catch (e) { return ""; }
}

// ---------------- network capture (IDM-like) ----------------

function addCapture(cap) {
  browser.storage.local.get({ [CAPTURES_KEY]: [] }).then((r) => {
    let list = r[CAPTURES_KEY] || [];
    const dup = list.find((c) => c.url === cap.url);
    if (dup) list = list.map((c) => (c.url === cap.url ? { ...cap, at: Date.now() } : c));
    else {
      list.unshift(cap);
      list = list.slice(0, 50);
    }
    browser.storage.local.set({ [CAPTURES_KEY]: list });
    const last = list[0];
    browser.runtime.sendMessage({ type: "file_captured", capture: last }).catch(() => {});
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

  // SILENT: IDM never shows OS toasts for detected links. Link detection only
  // updates the popup capture list — no OS notification is created here.
  // (Previous code calling browser.notifications.create(opts) is intentionally removed.)
  void (now, urlKey, hostKey, host);
}

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

browser.webRequest.onBeforeRequest.addListener(
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
  },
  { urls: ["<all_urls>"], types: ["media", "object"] },
  []
);

browser.webRequest.onHeadersReceived.addListener(
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
  },
  { urls: ["<all_urls>"], types: ["main_frame", "sub_frame", "other", "xmlhttprequest", "object"] },
  ["responseHeaders"]
);

setInterval(() => {
  capturedUrls = new Set([...capturedUrls].slice(-300));
}, 10 * 60 * 1000);

// ---------------- browser download takeover (IDM-style) ----------------

// A browser-initiated download counts as "ours" when the URL or suggested
// filename matches a real media/archive extension.
function isTakeoverUrl(item) {
  if (!item || !item.url || !item.url.startsWith("http")) return false;
  const fext = (item.filename || "").split(".").pop().toLowerCase().trim();
  const ext = extOf(item.url);
  return MEDIA_EXT.includes(ext) || FILE_EXT.includes(ext) ||
    MEDIA_EXT.includes(fext) || FILE_EXT.includes(fext);
}

// Merge request cookies for a set of URLs (download URL + referrer/page URL).
// More specific (longer) domains win for the same cookie name.
async function cookieHeaderFor(urls) {
  const seen = new Map();
  for (const u of urls) {
    if (!u || !u.startsWith("http")) continue;
    let cs = [];
    try {
      cs = (await browser.cookies.getAll({ url: u })) || [];
    } catch (e) { /* cookie access may fail for blob:/opaque origins */ }
    for (const c of cs) {
      const prev = seen.get(c.name);
      const prevDomain = prev && prev.domain;
      if (!prev || (c.domain && prevDomain && c.domain.length > prevDomain.length)) {
        seen.set(c.name, { domain: c.domain, value: c.value });
      } else if (!prev) {
        seen.set(c.name, { domain: c.domain, value: c.value });
      }
    }
  }
  const parts = [];
  for (const [name, c] of seen) if (c.value !== undefined && c.value !== "") parts.push(name + "=" + c.value);
  return parts.join("; ");
}

async function takeOverDownload(item) {
  const url = item.url;
  const filename = item.filename ? item.filename.split(/[\\/]/).pop() : undefined;
  const referrer = item.referrer || "";
  const cookies = await cookieHeaderFor([url, referrer]);
  // Cancel + erase the native browser download first so the page sees the
  // browser didn't save it — Vortex becomes the handler (IDM-style).
  try { await browser.downloads.cancel(item.id); } catch (e) {}
  try { await browser.downloads.erase({ id: item.id }); } catch (e) {}
  try { await browser.downloads.removeFile(item.id); } catch (e) {}
  // Hand the download to Vortex (no-op when the desktop is offline: the
  // browser naturally keeps its own download because we skip the handler).
  await handleStart({ type: "direct", url, filename, pageUrl: referrer, referer: referrer || undefined, cookies });
}

if (browser.downloads) {
  if (browser.downloads.onDeterminingFilename) {
    // Chrome: fire before the file is written so we can cancel cleanly.
    browser.downloads.onDeterminingFilename.addListener((item) => {
      if (!interceptEnabled || !online) return; // let the browser download normally
      if (!isTakeoverUrl(item)) return;
      takeOverDownload(item);
    });
  } else {
    // Firefox: no onDeterminingFilename — intercept just after creation.
    browser.downloads.onCreated.addListener((item) => {
      if (!interceptEnabled || !online) return;
      if (!isTakeoverUrl(item)) return;
      takeOverDownload(item);
    });
  }
}

// keep SW alive-ish: reconnect attempt every 15s when offline
setInterval(() => {
  if (!online) connect().catch(() => {});
}, 15000);

// ---------------- context menus ----------------

browser.runtime.onInstalled.addListener(async () => {
  await browser.contextMenus.removeAll();
  browser.contextMenus.create({
    id: "vx-download-link",
    title: "Download link with Vortex",
    contexts: ["link"],
  });
  browser.contextMenus.create({
    id: "vx-download-media",
    title: "Download this video/audio with Vortex",
    contexts: ["video", "audio"],
  });
  browser.contextMenus.create({
    id: "vx-download-page",
    title: "Download this page's video with Vortex",
    contexts: ["page"],
  });
  browser.contextMenus.create({
    id: "vx-grab-all",
    title: "Grab all links on this page with Vortex",
    contexts: ["page"],
  });
});

browser.contextMenus.onClicked.addListener((info, tab) => {
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
    const res = await browser.scripting.executeScript({
      target: { tabId: tab.id },
      func: collectPageLinks,
    });
    links = (res && res[0] && res[0].result) || [];
  } catch (e) {
    browser.notifications.create({
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
    browser.notifications.create({
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
  browser.notifications.create({
    type: "basic",
    iconUrl: "icons/icon128.png",
    title: "Vortex — grab all links",
    message: queued + " of " + links.length + " link(s) queued in Vortex",
    priority: 1,
  });
}

// ---------------- action dispatcher (used by content + popup + menus) ----------------

async function handleStart({ type, url, filename, pageUrl, referer, cookies }) {
  if (!url) return { error: "no url" };
  if (type === "direct" || isMediaSite(url) || type === "auto") {
    const isYT = isMediaSite(url);
    const isHLS = extOf(url) === "m3u8" || url.includes(".m3u8");
    const payload = { url, filename: filename || undefined };
    if (isYT || isHLS) payload.via = "yt";
    // Browser-takeover metadata so the desktop can replay authenticated downloads.
    if (referer) payload.referer = referer;
    if (cookies) payload.cookies = cookies;
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
    const q = { url };
    if (filename) q.filename = filename;
    if (referer) q.referer = referer;
    if (cookies) q.cookies = cookies;
    await launchVortex("capture", q);
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

browser.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  const tab = sender.tab;
  const pageUrl = (tab && tab.url) || "";
  const title = (tab && tab.title) || tabTitles.get(tab && tab.id) || "";

  if (msg.type === "ping") {
    sendResponse({ ok: isOnline(), running: online });
    return; // synchronous response
  }

  if (msg.type === "download_media") {
    const url = msg.url || "";
    // yt-dlp info probing can take 20-30s on slow/long pages — allow it.
    return safeRespond(
      sendResponse,
      async () => {
        if (url.startsWith("blob:") || isMediaSite(pageUrl)) {
          if (online) return { ...(await handleAnalyze(pageUrl)), action: "analyze" };
          await launchVortex("capture", { url: pageUrl, via: "yt" });
          return { ok: false, action: "analyze", launched: true, title };
        }
        return handleStart({ type: "direct", url, filename: msg.filename, pageUrl });
      },
      30000
    );
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
    return safeRespond(sendResponse, () => handleAnalyze(msg.url, sender.tab), 30000);
  }

  if (msg.type === "start_ytdl") {
    if (online) {
      return safeRespond(sendResponse, () => {
        // Explicit embed override so the popup toggle works both ways
        // (when OFF, we must send false — otherwise the desktop uses its own settings).
        const payload = { url: msg.url, format_id: msg.format_id, embed_subs: subsEnabled };
        return rpc("start_ytdl", payload);
      });
    }
    return safeRespond(sendResponse, async () => {
      await launchVortex("capture", { url: msg.url, via: "yt", format: msg.format_id });
      return { ok: false, launched: true };
    });
  }

  if (msg.type === "open_vortex") {
    return safeRespond(sendResponse, async () => ({ ok: await launchVortex("open", {}) }));
  }

  if (msg.type === "open_grabber") {
    return safeRespond(sendResponse, async () => {
      const url = msg.url || pageUrl || "";
      if (!/^https?:/i.test(url)) return { ok: false, error: "no http(s) url" };
      if (online) return rpc("open_grabber", { url });
      await launchVortex("open", {});
      return { ok: false, launched: true };
    }, 20000);
  }

  if (msg.type === "get_stats") {
    return safeRespond(sendResponse, () => rpc("stats", null, 8000), 9000);
  }

  if (msg.type === "list_captures") {
    return safeRespond(sendResponse, async () => {
      const r = await browser.storage.local.get({ [CAPTURES_KEY]: [] });
      return { captures: r[CAPTURES_KEY] || [], online };
    });
  }

  if (msg.type === "clear_captures") {
    return safeRespond(sendResponse, async () => {
      await browser.storage.local.set({ [CAPTURES_KEY]: [] });
      return { ok: true };
    });
  }

  // Unknown message type — never leave the caller hanging.
  sendResponse({ error: "unknown message: " + msg.type });
});

// wake the worker on tab navigation to keep WS fresh
browser.tabs.onUpdated.addListener((tabId) => {
  void tabId;
  if (!online) connect().catch(() => {});
});