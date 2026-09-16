// Vortex Companion — background service worker.
// Bridges the page/content-script with the desktop Vortex app over a local WebSocket.
// Also acts like an IDM extension: watches network traffic, grabs media/file URLs,
// context menus, and notifications.

const WS_ADDR = "ws://127.0.0.1:17190";
const MEDIA_EXT = ["mp4", "webm", "mov", "m4v", "mkv", "flv", "m4a", "mp3", "ogg", "oga", "opus", "wav", "aac", "flac", "m3u8"];
const FILE_EXT = ["zip", "rar", "7z", "tar", "gz", "bz2", "xz", "iso", "dmg", "exe", "msi", "apk", "deb", "rpm", "pdf", "epub", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "csv", "ttf", "otf", "dll", "torrent", "bin", "img", "ipa", "whl"];
const MEDIA_SITES = ["youtube.com", "youtu.be", "youtube-nocookie.com", "tiktok.com", "instagram.com", "facebook.com", "fb.watch", "twitter.com", "x.com", "dailymotion.com", "vimeo.com", "soundcloud.com", "bilibili.com", "twitch.tv", "reddit.com"];
const CAPTURES_KEY = "vx_captures";

let ws = null;
let wsReady = null; // promise controlling the current connection
let online = false;
let wsSerial = 0;
const pending = new Map(); // req -> {resolve, reject, timer}

const tabTitles = new Map();
chrome.tabs.onUpdated.addListener((tabId, changeInfo, tab) => {
  if (changeInfo.title) tabTitles.set(tabId, changeInfo.title);
  if (changeInfo.status === "loading" && tab.url) tabTitles.set(tabId, tab.title || "");
});

// ---------------- WebSocket core ----------------

function connect() {
  if (wsReady) return wsReady;
  wsReady = new Promise((resolve, reject) => {
    const sock = new WebSocket(WS_ADDR);
    ws = sock;
    sock.onopen = () => {
      online = true;
      resolve(true);
    };
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
    const fail = (e) => {
      online = false;
      ws = null;
      wsReady = null;
      reject(e);
    };
    sock.onerror = fail;
    sock.onclose = fail;
  });
  wsReady.catch(() => {});
  return wsReady;
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
      ws.send(JSON.stringify({ type, payload, req, ...(payload !== undefined ? {} : {}) }));
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
let lastNotify = 0;

function maybeNotify(cap) {
  const now = Date.now();
  if (now - lastNotify < 8000) return;
  lastNotify = now;
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
  const cd = (headers["content-disposition"] || "").toLowerCase();
  if (cd.includes("attachment")) return true;
  const ct = (headers["content-type"] || "").toLowerCase();
  const ext = extOf(url);
  if (FILE_EXT.includes(ext)) return true;
  if (MEDIA_EXT.includes(ext)) return true;
  if (cd.startsWith("inline")) return false;
  // binary-ish application types that are big
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
    if (online) return res;
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

chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  const tab = sender.tab;
  const pageUrl = (tab && tab.url) || "";
  const title = (tab && tab.title) || tabTitles.get(tab && tab.id) || "";

  if (msg.type === "ping") {
    const ok = isOnline();
    sendResponse({ ok, running: online });
    return;
  }

  if (msg.type === "download_media") {
    // media item from content script
    const url = msg.url;
    if (url.startsWith("blob:") || isMediaSite(pageUrl)) {
      if (online) {
        handleAnalyze(pageUrl).then((res) => sendResponse({ ...res, action: "analyze" }));
      } else {
        launchVortex("capture", { url: pageUrl, via: "yt" }).then(() =>
          sendResponse({ ok: false, action: "analyze", launched: true, title })
        );
      }
    } else {
      handleStart({ type: "direct", url, filename: msg.filename, pageUrl }).then(sendResponse);
    }
    return true;
  }

  if (msg.type === "download_direct") {
    handleStart({ type: "direct", url: msg.url, filename: msg.filename, pageUrl }).then(sendResponse);
    return true;
  }

  if (msg.type === "analyze") {
    handleAnalyze(msg.url, sender.tab).then(sendResponse);
    return true;
  }

  if (msg.type === "start_ytdl") {
    if (online) {
      rpc("start_ytdl", { url: msg.url, format_id: msg.format_id }).then(sendResponse);
    } else {
      launchVortex("capture", { url: msg.url, via: "yt", format: msg.format_id }).then(() =>
        sendResponse({ ok: false, launched: true })
      );
    }
    return true;
  }

  if (msg.type === "open_vortex") {
    launchVortex("open", {}).then((ok) => sendResponse({ ok }));
    return true;
  }

  if (msg.type === "get_stats") {
    rpc("stats", null, 8000).then(sendResponse);
    return true;
  }

  if (msg.type === "list_captures") {
    chrome.storage.local.get({ [CAPTURES_KEY]: [] }).then((r) =>
      sendResponse({ captures: r[CAPTURES_KEY] || [], online })
    );
    return true;
  }

  if (msg.type === "clear_captures") {
    chrome.storage.local.set({ [CAPTURES_KEY]: [] }).then(() => sendResponse({ ok: true }));
    return true;
  }
});

// wake the worker on tab navigation to keep WS fresh
chrome.tabs.onUpdated.addListener((tabId) => {
  void tabId;
  if (!online) connect().catch(() => {});
});