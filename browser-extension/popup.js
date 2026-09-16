// Vortex companion popup.

function $(id) {
  return document.getElementById(id);
}

function send(msg) {
  return new Promise((resolve) => {
    try {
      chrome.runtime.sendMessage(msg, (res) => resolve(res || {}));
    } catch (e) {
      resolve({ error: String(e) });
    }
  });
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
      if (on) {
        const res = await send({ type: "download_direct", url: c.url, filename: c.filename });
        if (res && res.launched) dl.textContent = "Launched ✓";
        else dl.textContent = "Started ✓";
      } else {
        await send({ type: "download_direct", url: c.url, filename: c.filename });
        dl.textContent = "Launched ✓";
      }
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
  const r = await send({ type: "download_direct", url, filename: "" });
  if (r && r.error && r.launched) btn.textContent = "Launched ✓";
  else btn.textContent = r && r.error ? "Failed: check Vortex" : "Started ✓";
  $("url").value = "";
  setTimeout(() => {
    btn.disabled = false;
    btn.textContent = "Add URL";
  }, 1400);
});
$("url").addEventListener("keydown", (e) => {
  if (e.key === "Enter") $("addbtn").click();
});

$("open").addEventListener("click", () => void send({ type: "open_vortex" }));
$("refresh").addEventListener("click", () => void refreshStatus());
$("clear").addEventListener("click", () => {
  void send({ type: "clear_captures" });
  $("cap-list").innerHTML = '<div class="empty">Watching for downloads on any tab…</div>';
});

refreshStatus();
setInterval(() => {
  void refreshStatus();
}, 5000);