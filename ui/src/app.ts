import { store, api } from "./lib/api";
import { icon, catIcon } from "./lib/icons";
import { toast } from "./lib/ui";
import { formatBytes, formatSpeed, formatEta } from "./lib/format";
import { SpeedChart } from "./lib/chart";
import type { CategoryId, Download } from "./types";
import { openAddUrl, openSettings, openConfirmRemove, openConfirmBulkRemove, openGrabber, openAuthDialog, openWelcome } from "./components/modals";
import { openYoutube } from "./components/youtube";

// When a download hits HTTP 401, pop the login dialog.
api.onAuthRequired = (p) => openAuthDialog(p);
// A playlist URL copied to the clipboard (with clipboard watch ON) opens the
// YouTube modal pre-filled — but does NOT auto-analyze: users often copy a
// link just to share it, so they decide with Analyze / Download / Cancel.
api.onPlaylistClip = (url) => openYoutube({ url, playlist: true });
// "Grab All Links on This Page" from the browser extension popup opens the
// Site Grabber modal pre-filled and starts the crawl automatically.
api.onGrabberOpen = (url) => openGrabber(url || "", true);

const CATS: { id: CategoryId; label: string; icon: "video" | "audio" | "doc" | "program" | "zip" | "other" }[] = [
  { id: "all", label: "All Downloads", icon: "other" },
  { id: "video", label: "Videos", icon: "video" },
  { id: "audio", label: "Audio", icon: "audio" },
  { id: "document", label: "Documents", icon: "doc" },
  { id: "program", label: "Programs", icon: "program" },
  { id: "zip", label: "ZIP", icon: "zip" },
  { id: "other", label: "Other", icon: "other" },
];

const STATUS_LABEL: Record<Download["status"], string> = {
  queued: "Queued",
  downloading: "Downloading",
  paused: "Paused",
  completed: "Completed",
  merging: "Merging",
  error: "Error",
  cancelled: "Cancelled",
  needs_auth: "Login required",
  resolving: "Resolving metadata…",
};

/** Row-sort keys tied to the clickable list headers + the quick date toggle. */
type SortColumn = "date" | "name" | "size" | "status";

/** Deterministic rank so STATUS sorts intuitively (active → merging → … → cancelled). */
const STATUS_RANK: Record<Download["status"], number> = {
  downloading: 0,
  merging: 1,
  queued: 2,
  resolving: 3,
  needs_auth: 4,
  paused: 5,
  error: 6,
  completed: 7,
  cancelled: 8,
};

/** First-click default order per column (name A–Z, size largest, status by rank, date newest). */
const SORT_DEFAULT: Record<SortColumn, "asc" | "desc"> = {
  date: "desc",
  name: "asc",
  size: "desc",
  status: "asc",
};

function esc(s: unknown): string {
  const t = typeof s === "string" ? s : s == null ? "" : String(s);
  return t.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}

/** Backend marks playlist jobs via filename `"k / N videos"`. Parse it for
 *  prominent display; null for single videos / history. Never throws. */
function playlistPos(filename: unknown): { k: number; n: number } | null {
  try {
    if (typeof filename !== "string") return null;
    const m = filename.match(/(\d+)\s*\/\s*(\d+)\s*videos/i);
    if (!m) return null;
    const k = Number(m[1]);
    const n = Number(m[2]);
    if (!Number.isFinite(k) || !Number.isFinite(n) || k <= 0 || n <= 0 || k > n) return null;
    return { k, n };
  } catch {
    return null;
  }
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/** "Done 17 Sep 2026 • 14:32" for finished downloads, "Added …" otherwise. */
function fmtStamp(d: Download): string {
  const ms = d.completed_at ?? d.created_at;
  if (!ms) return "";
  const dt = new Date(ms);
  const p = (n: number) => String(n).padStart(2, "0");
  const day = `${dt.getDate()} ${MONTHS[dt.getMonth()]} ${dt.getFullYear()}`;
  const time = `${p(dt.getHours())}:${p(dt.getMinutes())}`;
  return `${d.completed_at ? "Done" : "Added"} ${day} • ${time}`;
}

interface Desc {
  grid: string;
  headGrid: string;
}

export class VortexApp {
  private root: HTMLElement;
  private chart!: SpeedChart;
  private cat: CategoryId = "all";
  private search = "";
  private selected = new Set<string>();
  private lastSel = -1;
  private lastRowKey = "";
  private r1 = () => {};
  private r2 = () => {};
  private sort: { column: SortColumn; order: "asc" | "desc" } = { column: "date", order: "desc" };

  constructor() {
    this.root = document.getElementById("app")!;
  }

  mount() {
    // Render only when the list structure changes (status, add/remove, filter…).
    // Progress ticks keep the same key, so they take the cheap patchRows path.
    this.r1 = store.subscribe(() => {
      const k = this.rowKey();
      if (k === this.lastRowKey) this.patchRows();
      else {
        this.lastRowKey = k;
        this.render();
      }
    });
    this.renderShell();
    this.afterShell();
    this.render();
    this.r2 = this.startChartLoop();
  }

  private renderShell() {
    this.root.innerHTML = `
      ${this.titlebar()}
      ${this.toolbar()}
      <div class="middle">${this.sidebar()}${this.mainPanel()}</div>
      ${this.statusbar()}`;
  }

  private titlebar() {
    return `
    <div class="titlebar">
      <div class="brand"><img class="logo-img" src="/app-icon.png" alt="Vortex" />VORTEX</div>
      <span class="tagline">Download Manager</span>
      <div class="spacer"></div>
      <div class="tb-actions">
        <button data-win="minimize" title="Minimize">${icon("minimize", 15)}</button>
        <button data-win="toggle" title="Maximize">${icon("maximize", 14)}</button>
        <button data-win="close" class="close" title="Close">${icon("close", 15)}</button>
      </div>
    </div>`;
  }

  private toolbar() {
    const active = this.activeCount();
    return `
    <div class="toolbar">
      <button class="tbtn primary" id="tb-add">${icon("add", 16)} Add URL</button>
      <button class="tbtn" id="tb-yt">${icon("youtube", 16)} YouTube</button>
      <button class="tbtn" id="tb-grab">${icon("link", 16)} Grabber</button>
      <div class="divider"></div>
      <button class="tbtn" id="tb-pause" ${active ? "" : "disabled"}>${icon("pause", 15)} Pause</button>
      <button class="tbtn" id="tb-resume" ${active ? "" : "disabled"}>${icon("play", 15)} Resume</button>
      <button class="tbtn danger" id="tb-stop" ${active ? "" : "disabled"}>${icon("stop", 15)} Stop</button>
      <button class="tbtn" id="tb-retry">${icon("sync", 15)} Retry all</button>
      <div class="divider"></div>
      <button class="tbtn" id="tb-import">${icon("file", 15)} Import</button>
      <button class="tbtn" id="tb-settings">${icon("gear", 15)} Settings</button>
      <div class="grow"></div>
      <button class="tbtn" id="tb-sort" title="Sort by date (Newest / Oldest)">⇅ Newest</button>
      <div class="searchbox">
        ${icon("search", 14)}
        <input id="tb-search" placeholder="Search downloads…" spellcheck="false" value="${this.search}" />
      </div>
    </div>`;
  }

  private sidebar() {
    const counts: Record<string, number> = {};
    for (const d of store.downloads) counts[d.category] = (counts[d.category] ?? 0) + 1;
    counts.all = store.downloads.length;
    const tools = store.tools;
    return `
    <div class="sidebar">
      <div class="side-head">Categories</div>
      ${CATS.map(
        (c) => `
        <div class="side-item ${this.cat === c.id ? "active" : ""}" data-cat="${c.id}">
          ${icon(c.icon, 17)}
          <span>${c.label}</span>
          <span class="count">${counts[c.id] ?? 0}</span>
        </div>`,
      ).join("")}
      <div class="tools-status">
        <div class="side-head" style="padding:0">Tools</div>
        <div class="row"><span class="dot ${tools?.ytdlp ? "ok" : "missing"}"></span> yt-dlp ${tools?.ytdlp_version ? "v" + tools.ytdlp_version : tools?.ytdlp ? "" : "(not found)"}</div>
        <div class="row"><span class="dot ${tools?.ffmpeg ? "ok" : "missing"}"></span> ffmpeg ${tools?.ffmpeg_version ? "v" + tools.ffmpeg_version : tools?.ffmpeg ? "" : "(auto-download)"}</div>
        <button class="tbtn tools-update" id="tools-update">↻ Update Tools</button>
      </div>
    </div>`;
  }

  private mainPanel() {
    return `
    <div class="main">
      <div class="batch-emergency-bar" id="batch-bar">
        <span class="batch-msg">⚡ Batch in progress: <strong id="batch-active-count">0</strong> files</span>
        <div class="batch-actions">
          <button id="btn-batch-pause" class="btn-warning">⏸ Pause All</button>
          <button id="btn-batch-resume" class="btn-secondary">▶ Resume All</button>
          <button id="btn-batch-cancel" class="btn-danger">⏹ Stop &amp; Cancel All</button>
        </div>
      </div>
      <div class="list-head">
        <label class="dl-check"><input type="checkbox" id="check-all" title="Select all" /></label>
        <span data-sort="name" class="dl-sorthdr" title="Sort by file name">File<span class="dl-dir"></span></span>
        <span data-sort="size" class="dl-sorthdr" title="Sort by size">Size<span class="dl-dir"></span></span>
        <span>Progress</span><span class="dl-head-speed">Speed</span><span class="dl-head-eta">Time Left</span>
        <span data-sort="status" class="dl-sorthdr" title="Sort by status">Status<span class="dl-dir"></span></span>
        <span></span>
      </div>
      <div class="bulkbar" id="bulkbar">
        <span class="bulk-count" id="bulk-count">0 selected</span>
        <div class="bulk-actions">
          <button class="tbtn" data-bulk="resume">${icon("play", 14)} Resume</button>
          <button class="tbtn" data-bulk="pause">${icon("pause", 14)} Pause</button>
          <button class="tbtn" data-bulk="retry">${icon("sync", 14)} Retry</button>
          <button class="tbtn danger" data-bulk="remove">${icon("trash", 14)} Delete</button>
          <button class="tbtn" data-bulk="clear">${icon("close", 13)} Clear</button>
        </div>
      </div>
      <div class="list-scroll" id="list"></div>
      <div class="chartbar">
        <div class="chart-head">
          <span class="live"><b id="chart-speed" style="color:var(--text-1)">0 B/s</b> live transfer rate</span>
          <span id="chart-segs"></span>
        </div>
        <canvas id="speedChart"></canvas>
      </div>
    </div>`;
  }

  private statusbar() {
    const s = store.stats;
    return `
    <div class="statusbar">
      <div class="sv-item"><b>${s.active}</b> active</div>
      <div class="sv-item"><b class="live">${formatSpeed(s.total_speed)}</b> total speed</div>
      <div class="sv-item"><b>${s.segments}</b> segments</div>
      <div class="sv-item"><b>${formatBytes(s.total_downloaded)}</b> downloaded today</div>
      <div class="spacer" style="flex:1"></div>
      <div class="sv-item">${icon("file", 13)} <b>${s.completed}</b> completed</div>
    </div>`.replace('<div class="spacer" style="flex:1"></div>', '<div style="flex:1"></div>');
  }

  private activeCount(): number {
    return store.downloads.filter((d) => d.status === "downloading" || d.status === "queued" || d.status === "merging" || d.status === "resolving").length;
  }

  private visible(): Download[] {
    const q = this.search.toLowerCase();
    return store.downloads
      .filter((d) => (this.cat === "all" ? true : d.category === this.cat))
      .filter((d) => !q || (d.title + d.filename + d.url).toLowerCase().includes(q))
      .sort((a, b) => this.compare(a, b));
  }

  private compare(a: Download, b: Download): number {
    const dir = this.sort.order === "asc" ? 1 : -1;
    let cmp = 0;
    switch (this.sort.column) {
      case "date":
        cmp = (a.completed_at ?? a.created_at) - (b.completed_at ?? b.created_at);
        break;
      case "name":
        cmp = (a.title || a.filename).localeCompare(b.title || b.filename, undefined, { numeric: true });
        break;
      case "size":
        cmp = (a.total_size || a.downloaded) - (b.total_size || b.downloaded);
        break;
      case "status":
        cmp = STATUS_RANK[a.status] - STATUS_RANK[b.status];
        break;
    }
    return cmp * dir;
  }

  private setSort(col: SortColumn) {
    // Same column toggles direction; a new column uses its sensible first-click default.
    this.sort =
      this.sort.column === col
        ? { column: col, order: this.sort.order === "asc" ? "desc" : "asc" }
        : { column: col, order: SORT_DEFAULT[col] };
    this.updateSortChrome();
    this.render();
  }

  private updateSortChrome() {
    this.root.querySelectorAll<HTMLElement>(".list-head [data-sort] .dl-dir").forEach((el) => (el.textContent = ""));
    const arrow = this.root.querySelector<HTMLElement>(`.list-head [data-sort="${this.sort.column}"] .dl-dir`);
    if (arrow) arrow.textContent = this.sort.order === "asc" ? "▲" : "▼";
    const t = this.root.querySelector<HTMLButtonElement>("#tb-sort");
    if (t)
      t.textContent = this.sort.column === "date" && this.sort.order === "asc" ? "⇅ Oldest" : "⇅ Newest";
  }

  private row(d: Download): string {
    const dim = d.status === "completed" || d.status === "cancelled";
    const active = d.status === "downloading" || d.status === "merging" || d.status === "queued" || d.status === "resolving";
    const pct = Number.isFinite(d.progress) ? d.progress.toFixed(1) : "0.0";
    const barCls = d.status === "error" ? "err" : d.status === "completed" ? "done" : "";
    const checked = this.selected.has(d.id);
    const isTorrent = d.source === "torrent";
    const tx = isTorrent ? store.torrentExtra(d.id) : { up: 0, peers: d.live || 0, ratio: 0 };
    // Playlist prominence (A+): "▶ k/n • title" so the current video is
    // obvious; single videos / history fall through unchanged.
    const pl = d.source === "youtube" ? playlistPos(d.filename) : null;
    const nameHtml = pl ? `▶ ${pl.k}/${pl.n} • ${esc(d.title)}` : esc(d.title);
    const statusLabel =
      d.status === "merging" && pl ? `Merging ${pl.k}/${pl.n}…` : (STATUS_LABEL[d.status] ?? d.status);
    const cols = {
      check: `<label class="dl-check" title="Select"><input type="checkbox" data-check="${d.id}" ${checked ? "checked" : ""} /></label>`,
      name: `
        <div class="dl-name-cell">
          <span class="dl-ico">${catIcon(d.category, 18)}</span>
          <span style="min-width:0">
            <div class="dl-name" title="${nameHtml}">${nameHtml}</div>
            <div class="dl-sub">${esc(d.url)} ${d.source === "youtube" ? "• " + esc(d.filename) : ""}</div>
            <div class="dl-date">${fmtStamp(d)}</div>
          </span>
        </div>`,
      size: `<span class="dl-size">${formatBytes(d.total_size || d.downloaded)}</span>`,
      prog: `
        <div class="dl-prog">
          <div class="bar ${barCls}"><div class="fill" style="width:${pct}%"></div></div>
          <span class="dl-pct"><span class="dl-pct-val">${pct}%</span> <span class="dl-seg" style="color:var(--text-3)">${isTorrent ? `Peers ${tx.peers}` : `${d.segments} seg`}</span></span>
        </div>`,
      speed: `<span class="dl-speed">${d.status === "downloading" || d.status === "merging" ? (isTorrent ? `↓ ${formatSpeed(d.speed)} ↑ ${formatSpeed(tx.up)}` : formatSpeed(d.speed)) : "–"}</span>`,
      eta: `<span class="dl-eta">${d.status === "downloading" ? (isTorrent ? `Ratio ${tx.ratio.toFixed(2)}` : formatEta(d.eta)) : "–"}</span>`,
      status: `<div class="dl-status"><span class="chip ${d.status}">${statusLabel}${d.status === "error" && d.error ? ` • ${esc(d.error.slice(0, 28))}` : ""}</span></div>`,
      actions: `
        <div class="dl-actions">
          ${d.status === "downloading" || d.status === "queued" || d.status === "resolving" ? `<button data-act="pause" data-id="${d.id}" title="Pause">${icon("pause", 15)}</button>` : d.status === "paused" ? `<button data-act="resume" data-id="${d.id}" title="Resume">${icon("play", 15)}</button>` : ""}${d.status === "downloading" || d.status === "queued" || d.status === "paused" || d.status === "merging" || d.status === "resolving" ? `<button data-act="stop" data-id="${d.id}" title="Stop (keeps partial progress)">${icon("stop", 15)}</button>` : ""}
          ${d.status === "completed" ? `<button data-act="folder" data-id="${d.id}" title="Show in folder">${icon("folder", 15)}</button>` : ""}
          ${d.status === "completed" ? `<button data-act="open" data-id="${d.id}" title="Open file">${icon("play", 15)}</button>` : ""}
          ${d.source !== "youtube" && (d.status === "error" || d.status === "cancelled") ? `<button data-act="resume" data-id="${d.id}" title="Resume from partial progress">${icon("play", 15)}</button>` : ""}${d.status === "completed" || d.status === "needs_auth" || (d.source === "youtube" && (d.status === "error" || d.status === "cancelled")) ? `<button data-act="reload" data-id="${d.id}" title="Download again">${icon("redo", 15)}</button>` : ""}
          <button data-act="cancel" data-id="${d.id}" title="Remove">${icon("trash", 15)}</button>
        </div>`,
    };
    return `<div class="dl-row ${dim ? "dim" : ""} ${checked ? "sel" : ""} ${active ? "active" : ""}" data-row="${d.id}">${Object.values(cols).join("")}</div>`;
  }

  /** Structural signature of the visible list; progress/speed changes are excluded. */
  private rowKey(): string {
    const sel = [...this.selected].sort().join(",");
    const rows = store.downloads
      .map(
        (d) =>
          [d.id, d.status, d.source, d.category, d.filename, d.title ?? "", d.total_size, d.segments, d.error ?? "", d.completed_at ?? 0, d.created_at].join("~"),
      )
      .join("|");
    return `${this.cat}|${this.search}|${this.sort.column}:${this.sort.order}|${sel}|${rows}`;
  }

  /** Update only live metrics on already-rendered rows (no innerHTML rebuild → no flicker). */
  private patchRows() {
    const list = this.root.querySelector<HTMLElement>("#list");
    if (!list) return;
    const items = this.visible();
    const kids = Array.from(list.children).filter((c): c is HTMLElement => c instanceof HTMLElement && !!c.dataset.row);
    for (let i = 0; i < items.length && i < kids.length; i++) {
      const d = items[i];
      const el = kids[i];
      const fill = el.querySelector<HTMLElement>(".bar .fill");
      const pct = Number.isFinite(d.progress) ? d.progress.toFixed(1) : "0.0";
      if (fill) fill.style.width = `${pct}%`;
      const pv = el.querySelector<HTMLElement>(".dl-pct-val");
      if (pv) pv.textContent = `${pct}%`;
      const sz = el.querySelector<HTMLElement>(".dl-size");
      if (sz) sz.textContent = formatBytes(d.total_size || d.downloaded);
      const isT = d.source === "torrent";
      const txt = isT ? store.torrentExtra(d.id) : null;
      const segNote = el.querySelector<HTMLElement>(".dl-seg");
      if (segNote) segNote.textContent = isT ? `Peers ${txt!.peers}` : `${d.segments} seg`;
      const sp = el.querySelector<HTMLElement>(".dl-speed");
      if (sp)
        sp.textContent =
          d.status === "downloading" || d.status === "merging"
            ? isT
              ? `↓ ${formatSpeed(d.speed)} ↑ ${formatSpeed(txt!.up)}`
              : formatSpeed(d.speed)
            : "–";
      const eta = el.querySelector<HTMLElement>(".dl-eta");
      if (eta)
        eta.textContent =
          d.status === "downloading" ? (isT ? `Ratio ${txt!.ratio.toFixed(2)}` : formatEta(d.eta)) : "–";
    }
    this.refreshChrome();
  }

  private render() {
    const list = this.root.querySelector<HTMLElement>("#list");
    if (!list) return;
    this.lastRowKey = this.rowKey();
    // Drop selections for downloads that no longer exist.
    const alive = new Set(store.downloads.map((d) => d.id));
    for (const id of [...this.selected]) if (!alive.has(id)) this.selected.delete(id);
    const items = this.visible();
    list.innerHTML = items.length
      ? items.map((d) => this.row(d)).join("")
      : `
        <div class="empty">
          <div class="glow">${icon("download", 40)}</div>
          <h2>${this.search ? "No matching downloads" : this.cat === "all" ? "No downloads yet" : "This category is empty"}</h2>
          <p>${this.search ? "Try a different search." : "Paste a link and hit Add URL to begin."}</p>
        </div>`;

    // update sidebar counts + statusbar + toolbar active states
    this.updateBulk();
    this.refreshChrome();
  }

  private updateBulk() {
    const bar = this.root.querySelector<HTMLElement>("#bulkbar");
    if (!bar) return;
    const n = this.selected.size;
    bar.classList.toggle("show", n > 0);
    const c = this.root.querySelector<HTMLElement>("#bulk-count");
    if (c) c.textContent = `${n} selected`;
    const all = this.root.querySelector<HTMLInputElement>("#check-all");
    if (all) {
      const vis = this.visible();
      const selVis = vis.filter((d) => this.selected.has(d.id)).length;
      all.checked = vis.length > 0 && selVis === vis.length;
      all.indeterminate = selVis > 0 && selVis < vis.length;
    }
  }

  private toggleSel(id: string, shift = false) {
    const vis = this.visible();
    const idx = vis.findIndex((d) => d.id === id);
    if (idx < 0) return;
    if (shift && this.lastSel >= 0 && this.lastSel !== idx) {
      const [a, b] = this.lastSel < idx ? [this.lastSel, idx] : [idx, this.lastSel];
      const on = !this.selected.has(id);
      for (let i = a; i <= b; i++) {
        if (on) this.selected.add(vis[i].id);
        else this.selected.delete(vis[i].id);
      }
    } else if (this.selected.has(id)) {
      this.selected.delete(id);
    } else {
      this.selected.add(id);
    }
    this.lastSel = idx;
    this.render();
  }

  private async runBulk(action: "pause" | "resume" | "retry" | "remove") {
    const ids = [...this.selected];
    if (!ids.length) return;
    if (action === "remove") {
      openConfirmBulkRemove(ids.length, (del) => {
        void api.downloadsAction("remove", ids, del).then(() => {
          this.selected.clear();
          toast(`${ids.length} removed`, "ok");
        }).catch((e: unknown) => toast("Remove failed: " + String(e), "err"));
      });
      return;
    }
    try {
      const n = await api.downloadsAction(action, ids);
      this.selected.clear();
      toast(n ? `${n} download(s) ${action === "retry" ? "retried" : action + "d"}` : "Nothing to do", n ? "ok" : "info");
    } catch (e: unknown) {
      toast("Bulk action failed: " + String(e), "err");
    }
    await store.refresh();
  }

  private refreshChrome() {
    const counts: Record<string, number> = {};
    for (const d of store.downloads) counts[d.category] = (counts[d.category] ?? 0) + 1;
    this.root.querySelectorAll<HTMLElement>(".side-item").forEach((el) => {
      const c = el.dataset.cat!;
      const count = el.querySelector(".count");
      if (count) count.textContent = String(c === "all" ? store.downloads.length : counts[c] ?? 0);
    });
    const active = this.activeCount();
    const pa = this.root.querySelector<HTMLButtonElement>("#tb-pause");
    const re = this.root.querySelector<HTMLButtonElement>("#tb-resume");
    const st = this.root.querySelector<HTMLButtonElement>("#tb-stop");
    if (pa) pa.disabled = !active;
    if (re) re.disabled = !active;
    if (st) st.disabled = !active;
    // Emergency batch bar: visible whenever more than one download is live.
    const bat = this.root.querySelector<HTMLElement>("#batch-bar");
    const bcount = this.root.querySelector<HTMLElement>("#batch-active-count");
    if (bat && bcount) {
      bcount.textContent = String(active);
      bat.classList.toggle("show", active > 1);
    }
    const failed = store.downloads.filter((d) => d.status === "error" || d.status === "cancelled").length;
    const rt = this.root.querySelector<HTMLButtonElement>("#tb-retry");
    if (rt) rt.disabled = !failed;
    const s = store.stats;
    const bar = this.root.querySelector<HTMLElement>("#chart-speed");
    if (bar) bar.textContent = formatSpeed(s.total_speed);
    const segs = this.root.querySelector<HTMLElement>("#chart-segs");
    if (segs) segs.textContent = `${s.active} active • ${s.connections} connections`;
    this.root.querySelectorAll<HTMLElement>(".statusbar").forEach((el) => {
      const b = el.querySelectorAll("b");
      if (b.length >= 2) {
        b[0].textContent = String(s.active);
        b[1].textContent = formatSpeed(s.total_speed);
        b[2].textContent = String(s.segments);
        b[3].textContent = formatBytes(s.total_downloaded);
        b[4].textContent = String(s.completed);
      }
    });
  }

  private startChartLoop(): () => void {
    const canvas = this.root.querySelector<HTMLCanvasElement>("#speedChart");
    if (!canvas) return () => {};
    this.chart = new SpeedChart(canvas);
    void api.getTools().then((t) => {
      store.tools = t;
      this.refreshTools();
      // First-run onboarding: explain the red dots BEFORE the user panics,
      // and fetch missing tools automatically in the background.
      const missing = !t.ytdlp || !t.ffmpeg;
      try {
        if (!localStorage.getItem("vx_welcomed")) {
          localStorage.setItem("vx_welcomed", "1");
          openWelcome(missing, () => openSettings());
        } else if (missing) {
          toast("Helper tools missing — downloading in background…", "info");
        }
      } catch { /* private mode: skip onboarding */ }
      if (missing) {
        void api
          .updateTools()
          .then((res) => {
            store.tools = {
              ytdlp: res.ytdlp,
              ffmpeg: res.ffmpeg,
              ytdlp_version: res.ytdlp_version,
              ffmpeg_version: res.ffmpeg_version,
            };
            this.refreshTools();
            if (res.ytdlp && res.ffmpeg) toast("Helper tools ready", "ok");
          })
          .catch(() => toast("Tools download failed — press Update Tools to retry", "err"));
      }
    }).catch((e) => console.error("[tools status failed]", e));
    const id = setInterval(() => {
      this.chart.push(store.stats.total_speed);
    }, 700);
    return () => clearInterval(id);
  }

  private refreshTools() {
    const tools = store.tools;
    if (!tools) return;
    const el = this.root.querySelector<HTMLElement>(".tools-status");
    if (!el) return;
    el.innerHTML = `<div class="side-head" style="padding:0">Tools</div>
      <div class="row"><span class="dot ${tools.ytdlp ? "ok" : "missing"}"></span> yt-dlp ${tools.ytdlp_version ? "v" + tools.ytdlp_version : tools.ytdlp ? "" : "(not found)"}</div>
      <div class="row"><span class="dot ${tools.ffmpeg ? "ok" : "missing"}"></span> ffmpeg ${tools.ffmpeg_version ? "v" + tools.ffmpeg_version : tools.ffmpeg ? "" : "(auto-download)"}</div>
      <button class="tbtn tools-update" id="tools-update">↻ Update Tools</button>`;
    this.root.querySelector<HTMLButtonElement>(".tools-update")!.onclick = () => {
      void this.runToolsUpdate(el);
    };
  }

  private async runToolsUpdate(panel: HTMLElement) {
    const btn = panel.querySelector<HTMLButtonElement>(".tools-update");
    if (btn) {
      btn.disabled = true;
      btn.innerHTML = `<span class="spin"></span> Checking for updates…`;
    }
    try {
      const res = await api.updateTools();
      store.tools = {
        ytdlp: res.ytdlp,
        ffmpeg: res.ffmpeg,
        ytdlp_version: res.ytdlp_version,
        ffmpeg_version: res.ffmpeg_version,
      };
      const msg =
        res.message ||
        (res.updated ? "yt-dlp successfully updated" : "Tools are already up to date!");
      toast(msg, res.message?.toLowerCase().includes("timed out") ? "err" : res.updated ? "ok" : "info");
    } catch (e) {
      toast("Update check failed: " + String(e), "err");
    } finally {
      // Always re-render the panel so the spinner can never get stuck.
      this.refreshTools();
    }
  }

  private afterShell() {
    const bind = () => {
      this.root.querySelectorAll<HTMLElement>("[data-win]").forEach((b) => {
        b.onclick = () => api.windowAction(b.dataset.win as "minimize" | "toggle" | "close");
      });
      // Guarded bindings: one missing toolbar id must never abort the
      // whole bind (which would silently kill pause/resume/search/list).
      const tbAdd = this.root.querySelector<HTMLButtonElement>("#tb-add");
      if (tbAdd) tbAdd.onclick = openAddUrl;
      const tbYt = this.root.querySelector<HTMLButtonElement>("#tb-yt");
      if (tbYt) tbYt.onclick = () => openYoutube(undefined);
      const tbGrab = this.root.querySelector<HTMLButtonElement>("#tb-grab");
      if (tbGrab) tbGrab.onclick = () => openGrabber(undefined);
      const tbImport = this.root.querySelector<HTMLButtonElement>("#tb-import");
      if (tbImport)
        tbImport.onclick = async () => {
          try {
            const urls = await api.readUrls();
            if (!urls.length) return toast("No URLs found", "err");
            for (const u of urls) {
              await api.startDownload(u, store.settings?.path || "", store.settings?.segments ?? 8);
            }
            toast(`${urls.length} download(s) queued`, "ok");
          } catch (e: unknown) {
            toast("Import failed: " + String(e), "err");
          }
        };
      const tbSettings = this.root.querySelector<HTMLButtonElement>("#tb-settings");
      if (tbSettings) tbSettings.onclick = openSettings;
      const tbSort = this.root.querySelector<HTMLButtonElement>("#tb-sort");
      if (tbSort) tbSort.onclick = () => this.setSort("date");
      this.root.querySelectorAll<HTMLElement>(".list-head [data-sort]").forEach((el) => {
        el.onclick = () => this.setSort(el.dataset.sort as SortColumn);
      });
      this.updateSortChrome();
      const pa = this.root.querySelector<HTMLButtonElement>("#tb-pause");
      const re = this.root.querySelector<HTMLButtonElement>("#tb-resume");
      if (pa) pa.onclick = async () => {
        try {
          const n = await api.pauseAllDownloads();
          toast(n ? `${n} download(s) paused` : "Nothing to pause", n ? "ok" : "info");
        } catch (e: unknown) {
          toast("Pause failed: " + String(e), "err");
        }
      };
      if (re) re.onclick = async () => {
        try {
          const n = await api.resumeAllDownloads();
          toast(n ? `${n} download(s) resumed` : "Nothing to resume", n ? "ok" : "info");
        } catch (e: unknown) {
          toast("Resume failed: " + String(e), "err");
        }
      };
      const stop = this.root.querySelector<HTMLButtonElement>("#tb-stop");
      if (stop) stop.onclick = async () => {
        try {
          const n = await api.cancelAllActive();
          toast(n ? `Stopped ${n} download(s)` : "Nothing active", n ? "ok" : "info");
          await store.refresh();
        } catch (e: unknown) {
          toast("Stop failed: " + String(e), "err");
        }
      };
      // Emergency batch bar actions (pause/resume/cancel everything at once).
      const bpause = this.root.querySelector<HTMLButtonElement>("#btn-batch-pause");
      const bresume = this.root.querySelector<HTMLButtonElement>("#btn-batch-resume");
      const bcancel = this.root.querySelector<HTMLButtonElement>("#btn-batch-cancel");
      if (bpause)
        bpause.onclick = async () => {
          try {
            const n = await api.pauseAllDownloads();
            toast(n ? `${n} download(s) paused` : "Nothing to pause", n ? "ok" : "info");
          } catch (e: unknown) {
            toast("Pause failed: " + String(e), "err");
          }
        };
      if (bresume)
        bresume.onclick = async () => {
          try {
            const n = await api.resumeAllDownloads();
            toast(n ? `${n} download(s) resumed` : "Nothing to resume", n ? "ok" : "info");
          } catch (e: unknown) {
            toast("Resume failed: " + String(e), "err");
          }
        };
      if (bcancel)
        bcancel.onclick = async () => {
          try {
            const n = await api.cancelAllActive();
            toast(n ? `Stopped & cancelled ${n} download(s)` : "Nothing active", n ? "ok" : "info");
            await store.refresh();
          } catch (e: unknown) {
            toast("Stop failed: " + String(e), "err");
          }
        };
      const rt = this.root.querySelector<HTMLButtonElement>("#tb-retry");
      if (rt) rt.onclick = async () => {
        try {
          const n = await api.retryAllDownloads();
          toast(n ? `${n} download(s) retried` : "Nothing to retry", n ? "ok" : "info");
        } catch (e: unknown) {
          toast("Retry failed: " + String(e), "err");
        }
      };
      this.root.querySelectorAll<HTMLElement>(".side-item").forEach((el) => {
        el.onclick = () => {
          this.cat = el.dataset.cat as CategoryId;
          this.root.querySelectorAll(".side-item").forEach((x) => x.classList.remove("active"));
          el.classList.add("active");
          this.render();
        };
      });
      const search = this.root.querySelector<HTMLInputElement>("#tb-search");
      if (search) search.value = this.search;
      if (search)
        search.addEventListener("input", () => {
          this.search = search.value;
          this.render();
        });
      const checkAll = this.root.querySelector<HTMLInputElement>("#check-all");
      if (checkAll)
        checkAll.addEventListener("change", () => {
          const vis = this.visible();
          if (checkAll.checked) vis.forEach((d) => this.selected.add(d.id));
          else vis.forEach((d) => this.selected.delete(d.id));
          this.render();
        });
      const bulkbar = this.root.querySelector<HTMLElement>("#bulkbar");
      if (bulkbar)
        bulkbar.addEventListener("click", (e) => {
          const b = (e.target as HTMLElement).closest("[data-bulk]") as HTMLElement | null;
          if (!b) return;
          const a = b.dataset.bulk!;
          if (a === "clear") {
            this.selected.clear();
            this.render();
            return;
          }
          void this.runBulk(a as "pause" | "resume" | "retry" | "remove");
        });
      const listEl = this.root.querySelector<HTMLElement>("#list");
      if (listEl)
        listEl.addEventListener("click", (e) => {
        const target = e.target as HTMLElement;
        const actEl = target.closest("[data-act]") as HTMLElement | null;
        if (!actEl) {
          const rowEl = target.closest("[data-row]") as HTMLElement | null;
          if (rowEl?.dataset.row) this.toggleSel(rowEl.dataset.row, (e as MouseEvent).shiftKey);
          return;
        }
        const t = actEl;
        const id = t.dataset.id!;
        const act = t.dataset.act!;
        const d = store.downloads.find((x) => x.id === id);
        if (!d) return;
        const isTorrent = d.source === "torrent";
        // Row actions: failures toast (no silent dead clicks); chained list
        // refreshes stay quiet (next tick/event covers them anyway).
        const quiet = (p: Promise<unknown>) =>
          p.catch((e: unknown) => console.error("[row refresh]", e));
        const run = (p: Promise<unknown>, what: string) =>
          p.catch((e: unknown) => toast(`${what} failed: ${String(e)}`, "err"));
        if (act === "pause") {
          if (isTorrent) run(api.pauseTorrent(id).then(() => quiet(store.refresh())), "Pause");
          else run(api.pauseDownload(id), "Pause");
        } else if (act === "resume") {
          if (isTorrent) run(api.resumeTorrent(id).then(() => quiet(store.refresh())), "Resume");
          else run(api.resumeDownload(id), "Resume");
        } else if (act === "stop") {
          if (isTorrent) run(api.cancelTorrent(id, false).then(() => quiet(store.refresh())), "Stop");
          else run(api.cancelDownload(id).then(() => quiet(store.refresh())), "Stop");
        } else if (act === "folder") run(api.openFolder(d.save_path), "Open folder");
        else if (act === "open") run(api.openFile(d.save_path), "Open file");
        else if (act === "reload") {
          if (isTorrent) {
            // Re-seed / restart the same session task.
            run(api.resumeTorrent(id).then(() => quiet(store.refresh())), "Reload");
          } else if (d.source === "youtube" || d.status === "completed") {
            if (d.source === "youtube") {
              const path = store.settings?.path || "";
              if (d.format_id) {
                run(api.startYtdl(d.url, d.format_id, path, false, undefined), "Download");
              } else {
                run(api.fetchYtdlInfo(d.url).then((info) => {
                  const best = info.formats.find((f) => f.note?.includes("Best")) ?? info.formats[0];
                  if (best) return api.startYtdl(d.url, best.id, path, false, undefined);
                }), "Download");
              }
            } else {
              run(api.startDownload(d.url, store.settings?.path || "", store.settings?.segments ?? 8), "Download");
            }
          } else if (isTorrent) {
            // Torrent error/cancelled: unpause the SAME session task.
            run(api.resumeTorrent(id).then(() => quiet(store.refresh())), "Resume");
          } else {
            // HTTP error/cancelled: resume the SAME task so kept part files
            // continue instead of downloading from zero.
            run(api.resumeDownload(id), "Resume");
          }
        } else if (act === "cancel") {
          if (d.status === "completed") {
            openConfirmRemove(d.filename || d.title, (del) => run(api.removeDownload(id, del), "Remove"));
          } else {
            run(api.removeDownload(id, false), "Remove");
          }
        }
      });
      // Drag & drop URL import. Torrents are not supported in this version.
      document.addEventListener("dragover", (e) => e.preventDefault());
      document.addEventListener("drop", (e) => {
        e.preventDefault();
        const txt =
          e.dataTransfer?.getData("text/plain") ||
          e.dataTransfer?.getData("text/uri-list") ||
          e.dataTransfer?.getData("URL") ||
          "";
        const parts = txt
          .split(/[\s,;]+/)
          .map((u) => u.trim())
          .filter(Boolean);
        const magnets = parts.filter((u) => /^magnet:/i.test(u));
        const urls = parts.filter((u) => u.startsWith("http://") || u.startsWith("https://"));
        if (magnets.length) toast("Torrent downloads are not supported in this version", "err");
        if (!urls.length) return;
        for (const u of urls) {
          void api.startDownload(u, store.settings?.path || "", store.settings?.segments ?? 8).catch((err) =>
            toast(String(err), "err"),
          );
        }
        toast(`${urls.length} URL(s) added from drop`, "ok");
      });
    };
    bind();
  }

  dispose(): void {
    this.r1();
    this.r2();
  }
}