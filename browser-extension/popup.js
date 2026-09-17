// Vortex companion popup.

// Dual-browser support (see background.js): alias the Firefox `browser.*`
// namespace for Chrome so the same code runs on both.
if (typeof browser === "undefined" && typeof globalThis.chrome !== "undefined") {
  var browser = globalThis.chrome;
}

function $(id) {
  return document.getElementById(id);
}

// Tiny inline toast (bottom of the popup).
function toast(msg, ms = 2800) {
  let el = $("vx-toast");
  if (!el) {
    el = document.createElement("div");
    el.id = "vx-toast";
    document.body.appendChild(el);
  }
  el.textContent = msg;
  el.classList.add("show");
  clearTimeout(el._t);
  el._t = setTimeout(() => el.classList.remove("show"), ms);
}

// Send a message to the background event page. Always resolves (never hangs):
// a timeout guards against a missing/closed bridge.
function send(msg, timeoutMs = 8000) {
  const timeout = new Promise((resolve) =>
    setTimeout(() => resolve({ error: "No response from Vortex bridge (timed out)" }), timeoutMs)
  );
  const call = browser.runtime.sendMessage(msg).then(
    (res) => res || {},
    (e) => ({ error: (e && e.message) || String(e) })
  );
  return Promise.race([call, timeout]);
}

function fmtBytes(n) {
  if (!n) return "0 B";
  if (n > 1073741824) return (n / 1073741824).toFixed(2) + " GB";
  if (n > 1048576) return (n / 1048576).toFixed(1) + " MB";
  if (n > 1024) return (n / 1024).toFixed(0) + " kB";
  return n + " B";
}
function esc(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

async function refreshStatus() {
  const st = $("status");
  const p = await send({ type: "ping" });
  const on = p && p.running;
  st.textContent = on ? "Connected to Vortex" : "Vortex not running";
  st.className = "status " + (on ? "on" : "off");

  const statsEl = $("stats");
  if (on) {
    const r = await send({ type: "get_stats" });
    const s = r.stats || {};
    $("st-active").textContent = s.active ?? 0;
    $("st-speed").textContent = fmtBytes(s.total_speed);
    $("st-done").textContent = s.completed ?? 0;
    statsEl.classList.remove("hidden");
  } else {
    statsEl.classList.add("hidden");
  }
  loadCaptures(on);
  return on;
}

async function loadCaptures(on) {
  const r = await send({ type: "list_captures" });
  const list = (r.captures || []).slice(0, 12);
  const box = $("cap-list");
  if (!list.length) {
    box.innerHTML = '<div class="empty">Watching for downloads on any tab…</div>';
    return;
  }
  box.innerHTML = "";
  for (const c of list) {
    const row = document.createElement("div");
    row.className = "cap";
    const name = document.createElement("div");
    name.className = "n";
    name.textContent = c.filename || c.url;
    name.title = c.url;
    const k = document.createElement("div");
    k.className = "k";
    k.textContent = c.kind === "hls" ? "HLS" : c.kind;
    const dl = document.createElement("button");
    dl.className = "btn primary";
    dl.style.padding = "5px 9px";
    dl.style.fontSize = "11px";
    dl.textContent = "Download";
    dl.disabled = !on;
    dl.addEventListener("click", async () => {
      dl.disabled = true;
      dl.textContent = "Sending…";
      const res = await send(
        { type: "download_direct", url: c.url, filename: c.filename },
        5000
      );
      if (res && res.error && !res.launched) {
        dl.disabled = false;
        dl.textContent = "Failed";
        alert("Could not send to Vortex:\n" + res.error);
        setTimeout(() => (dl.textContent = "Download"), 1500);
        return;
      }
      dl.textContent = res && res.launched ? "Launched ✓" : "Started ✓";
      setTimeout(() => dl.remove(), 1400);
    });
    row.appendChild(name);
    row.appendChild(k);
    row.appendChild(dl);
    box.appendChild(row);
  }
}

$("addbtn").addEventListener("click", async () => {
  const url = $("url").value.trim();
  if (!url) return;
  const btn = $("addbtn");
  btn.disabled = true;
  btn.textContent = "Sending…";
  const reset = () => {
    btn.disabled = false;
    btn.textContent = "Add URL";
  };
  // 5-second cap: never stay stuck on "Sending…".
  const r = await send({ type: "download_direct", url, filename: "" }, 5000);
  if (r && r.error && !r.launched) {
    btn.textContent = "Failed — check Vortex";
    alert("Could not send to Vortex:\n" + r.error);
    setTimeout(reset, 1800);
    return;
  }
  btn.textContent = r && r.launched ? "Launched ✓" : "Started ✓";
  $("url").value = "";
  setTimeout(reset, 1400);
});
$("url").addEventListener("keydown", (e) => {
  if (e.key === "Enter") $("addbtn").click();
});

// ---- Grab all links on the active page → opens Vortex's Site Grabber ----
$("grab-page").addEventListener("click", async () => {
  const btn = $("grab-page");
  btn.disabled = true;
  btn.textContent = "Grabbing links from page…";
  const reset = () => {
    btn.disabled = false;
    btn.textContent = "🔗 Grab All Links on This Page";
  };
  let pageUrl = "";
  try {
    const tabs = await browser.tabs.query({ active: true, currentWindow: true });
    pageUrl = (tabs && tabs[0] && tabs[0].url) || "";
  } catch (e) { /* ignore */ }
  if (!/^https?:/i.test(pageUrl)) {
    toast("Open a page you want to grab first.");
    reset();
    return;
  }
  toast("Grabbing links from page…");
  try {
    const r = await send({ type: "open_grabber", url: pageUrl }, 15000);
    reset();
    const ok = !!(r && (r.success || r.launched));
    if (!ok) {
      toast("Could not open Grabber: " + ((r && r.error) || "unknown"));
      return;
    }
    try {
      const host = new URL(pageUrl).hostname;
      toast("Grabber opened for " + host);
    } catch (e) {
      toast("Grabber opened — check the Vortex window.");
    }
  } catch (e) {
    reset();
    toast("Could not reach Vortex.");
  }
});

$("open").addEventListener("click", () => void send({ type: "open_vortex" }));
$("refresh").addEventListener("click", () => void refreshStatus());
$("clear").addEventListener("click", () => {
  void send({ type: "clear_captures" });
  $("cap-list").innerHTML = '<div class="empty">Watching for downloads on any tab…</div>';
});

// ---- Pairing key ----
(async () => {
  const stored = (await browser.storage.local.get({ vx_token: "" })).vx_token;
  const pairInput = $("pair-key");
  if (stored) {
    pairInput.value = stored;
    pairInput.placeholder = "Key saved ✓";
  }
})();

$("pair-save").addEventListener("click", async () => {
  const key = $("pair-key").value.trim();
  if (!key) return;
  await browser.storage.local.set({ vx_token: key });
  $("pair-key").placeholder = "Key saved ✓";
  // Force reconnection with the new key
  $("pair-save").textContent = "Saved ✓";
  setTimeout(() => { $("pair-save").textContent = "Save"; }, 1200);
  void refreshStatus();
});

$("pair-clear").addEventListener("click", async () => {
  await browser.storage.local.remove("vx_token");
  $("pair-key").value = "";
  $("pair-key").placeholder = "Paste key from Vortex → Settings";
});

// ---- Notification toggle (DEFAULT: off — link detection is silent) ----
(async () => {
  const { vx_notify } = await browser.storage.local.get({ vx_notify: false });
  $("notify-toggle").checked = vx_notify === true;
})();

$("notify-toggle").addEventListener("change", async (e) => {
  await browser.storage.local.set({ vx_notify: !!e.target.checked });
});

// ---- Intercept toggle (DEFAULT: on — Vortex becomes the default downloader) ----
(async () => {
  const { vx_intercept } = await browser.storage.local.get({ vx_intercept: true });
  $("intercept-toggle").checked = vx_intercept !== false;
})();

$("intercept-toggle").addEventListener("change", async (e) => {
  await browser.storage.local.set({ vx_intercept: !!e.target.checked });
});

// ---- Subtitle embed toggle ----
(async () => {
  const { vx_subs_embed } = await browser.storage.local.get({ vx_subs_embed: true });
  $("subs-toggle").checked = vx_subs_embed !== false;
})();

$("subs-toggle").addEventListener("change", async (e) => {
  await browser.storage.local.set({ vx_subs_embed: !!e.target.checked });
});

// ---- Hover bar opacity (20–100%, DEFAULT: 100 = solid) ----
function hbarOpacityLabel(v) {
  $("hbar-opacity-val").textContent = v + "%";
}
(async () => {
  const { vx_hbar_opacity } = await browser.storage.local.get({ vx_hbar_opacity: 100 });
  const v = Math.min(100, Math.max(20, Number(vx_hbar_opacity) || 100));
  $("hbar-opacity").value = v;
  hbarOpacityLabel(v);
})();

$("hbar-opacity").addEventListener("input", async (e) => {
  const v = Math.min(100, Math.max(20, Number(e.target.value) || 100));
  hbarOpacityLabel(v);
  await browser.storage.local.set({ vx_hbar_opacity: v });
});

// ---- Standalone subtitle download (.srt / .vtt only) ----
let popSubFmt = "srt";
function setSubFmt(f) {
  popSubFmt = f;
  $("subfmt-srt").classList.toggle("on", f === "srt");
  $("subfmt-vtt").classList.toggle("on", f === "vtt");
  document.querySelectorAll("#subs-list [data-sub]").forEach((b) => (b.textContent = "." + f));
}
$("subfmt-srt").addEventListener("click", () => setSubFmt("srt"));
$("subfmt-vtt").addEventListener("click", () => setSubFmt("vtt"));

$("subs-fetch").addEventListener("click", async () => {
  const btn = $("subs-fetch");
  const box = $("subs-list");
  btn.disabled = true;
  btn.textContent = "Fetching…";
  box.innerHTML = "";
  const tabs = await browser.tabs.query({ active: true, currentWindow: true });
  const tab = tabs && tabs[0];
  const url = tab && tab.url ? tab.url : "";
  const reset = (t) => {
    btn.disabled = false;
    btn.textContent = t || "List subtitles for current tab";
  };
  if (!/^https?:/i.test(url)) {
    box.innerHTML = '<div class="empty">Open a video page first.</div>';
    reset();
    return;
  }
  const res = await send({ type: "analyze", url }, 30000);
  const subs = ((res && res.info && res.info.subtitles) || []).filter((s) => !s.auto);
  if (!subs.length) {
    box.innerHTML =
      '<div class="empty">' +
      (res && res.error ? "Error: " + esc(res.error) : "No official subtitles found.") +
      "</div>";
    reset();
    return;
  }
  reset("Refresh subtitles");
  for (const s of subs.slice(0, 20)) {
    const row = document.createElement("div");
    row.className = "cap";
    const name = document.createElement("div");
    name.className = "n";
    name.textContent = s.label || s.lang;
    name.title = s.lang;
    const dl = document.createElement("button");
    dl.className = "btn primary";
    dl.style.padding = "5px 9px";
    dl.style.fontSize = "11px";
    dl.dataset.sub = s.lang;
    dl.textContent = "." + popSubFmt;
    dl.addEventListener("click", async () => {
      dl.disabled = true;
      dl.textContent = "Sending…";
      const r = await send(
        { type: "start_ytdl", url, format_id: "subs:" + popSubFmt + ":" + s.lang },
        8000
      );
      if (r && r.error && !r.launched) {
        dl.disabled = false;
        dl.textContent = "Failed";
        alert("Could not download subtitle:\n" + r.error);
        setTimeout(() => (dl.textContent = "." + popSubFmt), 1600);
        return;
      }
      dl.textContent = "Saved ✓";
      setTimeout(() => {
        dl.disabled = false;
        dl.textContent = "." + popSubFmt;
      }, 1600);
    });
    row.appendChild(name);
    row.appendChild(dl);
    box.appendChild(row);
  }
});

refreshStatus();
setInterval(() => {
  void refreshStatus();
}, 5000);