import { store, api } from "./lib/api";
import { icon, catIcon } from "./lib/icons";
import { toast } from "./lib/ui";
import { formatBytes, formatSpeed, formatEta } from "./lib/format";
import { SpeedChart } from "./lib/chart";
import type { CategoryId, Download } from "./types";
import { openAddUrl, openSettings, openConfirmRemove, openConfirmBulkRemove, openGrabber, openAuthDialog } from "./components/modals";
import { openYoutube } from "./components/youtube";

// When a download hits HTTP 401, pop the login dialog.
api.onAuthRequired = (p) => openAuthDialog(p);

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
};

function esc(s: string): string {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
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
  private r1 = () => {};
  private r2 = () => {};

  constructor() {
    this.root = document.getElementById("app")!;
  }

  mount() {
    this.r1 = store.subscribe(() => this.render());
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
      <div class="brand"><span class="logo">${icon("logo", 14)}</span>VORTEX</div>
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
      <button class="tbtn" id="tb-retry">${icon("sync", 15)} Retry all</button>
      <div class="divider"></div>
      <button class="tbtn" id="tb-import">${icon("file", 15)} Import</button>
      <button class="tbtn" id="tb-settings">${icon("gear", 15)} Settings</button>
      <div class="grow"></div>
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
      <div class="list-head">
        <label class="dl-check"><input type="checkbox" id="check-all" title="Select all" /></label>
        <span>File</span><span>Size</span><span>Progress</span><span class="dl-head-speed">Speed</span><span class="dl-head-eta">Time Left</span><span>Status</span><span></span>
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
    return store.downloads.filter((d) => d.status === "downloading" || d.status === "queued" || d.status === "merging").length;
  }

  private visible(): Download[] {
    const q = this.search.toLowerCase();
    return store.downloads
      .filter((d) => (this.cat === "all" ? true : d.category === this.cat))
      .filter((d) => !q || (d.title + d.filename + d.url).toLowerCase().includes(q))
      .sort((a, b) => b.created_at - a.created_at);
  }

  private row(d: Download): string {
    const dim = d.status === "completed" || d.status === "cancelled";
    const pct = d.progress.toFixed(1);
    const barCls = d.status === "error" ? "err" : d.status === "completed" ? "done" : "";
    const checked = this.selected.has(d.id);
    const cols = {
      check: `<label class="dl-check" title="Select"><input type="checkbox" data-check="${d.id}" ${checked ? "checked" : ""} /></label>`,
      name: `
        <div class="dl-name-cell">
          <span class="dl-ico">${catIcon(d.category, 18)}</span>
          <span style="min-width:0">
            <div class="dl-name" title="${esc(d.title)}">${esc(d.title)}</div>
            <div class="dl-sub">${esc(d.url)} ${d.source === "youtube" ? "• " + esc(d.filename) : ""}</div>
          </span>
        </div>`,
      size: `<span class="dl-size">${formatBytes(d.total_size || d.downloaded)}</span>`,
      prog: `
        <div class="dl-prog">
          <div class="bar ${barCls}"><div class="fill" style="width:${pct}%"></div></div>
          <span class="dl-pct">${pct}% <span style="color:var(--text-3)">${d.segments} seg</span></span>
        </div>`,
      speed: `<span class="dl-speed">${d.status === "downloading" || d.status === "merging" ? formatSpeed(d.speed) : "–"}</span>`,
      eta: `<span class="dl-eta">${d.status === "downloading" ? formatEta(d.eta) : "–"}</span>`,
      status: `<div class="dl-status"><span class="chip ${d.status}">${STATUS_LABEL[d.status]}${d.status === "error" && d.error ? ` • ${esc(d.error.slice(0, 28))}` : ""}</span></div>`,
      actions: `
        <div class="dl-actions">
          ${d.status === "downloading" || d.status === "queued" ? `<button data-act="pause" data-id="${d.id}" title="Pause">${icon("pause", 15)}</button>` : d.status === "paused" ? `<button data-act="resume" data-id="${d.id}" title="Resume">${icon("play", 15)}</button>` : ""}
          ${d.status === "completed" ? `<button data-act="folder" data-id="${d.id}" title="Show in folder">${icon("folder", 15)}</button>` : ""}
          ${d.status === "completed" ? `<button data-act="open" data-id="${d.id}" title="Open file">${icon("play", 15)}</button>` : ""}
          ${d.status === "completed" || d.status === "error" || d.status === "cancelled" || d.status === "needs_auth" ? `<button data-act="reload" data-id="${d.id}" title="Download again">${icon("redo", 15)}</button>` : ""}
          <button data-act="cancel" data-id="${d.id}" title="Remove">${icon("trash", 15)}</button>
        </div>`,
    };
    return `<div class="dl-row ${dim ? "dim" : ""} ${checked ? "sel" : ""}" data-row="${d.id}">${Object.values(cols).join("")}</div>`;
  }

  private render() {
    const list = this.root.querySelector<HTMLElement>("#list");
    if (!list) return;
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
        });
      });
      return;
    }
    const n = await api.downloadsAction(action, ids);
    this.selected.clear();
    toast(n ? `${n} download(s) ${action === "retry" ? "retried" : action + "d"}` : "Nothing to do", n ? "ok" : "info");
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
    if (pa) pa.disabled = !active;
    if (re) re.disabled = !active;
    const failed = store.downloads.filter((d) => d.status === "error" || d.status === "cancelled").length;
    const rt = this.root.querySelector<HTMLButtonElement>("#tb-retry");
    if (rt) rt.disabled = !failed;
    const s = store.stats;
    const bar = this.root.querySelector<HTMLElement>("#chart-speed");
    if (bar) bar.textContent = formatSpeed(s.total_speed);
    const segs = this.root.querySelector<HTMLElement>("#chart-segs");
    if (segs) segs.textContent = `${s.active} active • ${s.segments} connections`;
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
    const canvas = this.root.querySelector<HTMLCanvasElement>("#speedChart")!;
    this.chart = new SpeedChart(canvas);
    void api.getTools().then((t) => {
      store.tools = t;
      this.refreshTools();
    });
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
      this.root.querySelector<HTMLButtonElement>("#tb-add")!.onclick = openAddUrl;
      this.root.querySelector<HTMLButtonElement>("#tb-yt")!.onclick = openYoutube;
      this.root.querySelector<HTMLButtonElement>("#tb-grab")!.onclick = openGrabber;
      this.root.querySelector<HTMLButtonElement>("#tb-import")!.onclick = async () => {
        const urls = await api.readUrls();
        if (!urls.length) return toast("No URLs found", "err");
        for (const u of urls) {
          await api.startDownload(u, store.settings?.path || "", store.settings?.segments ?? 8);
        }
        toast(`${urls.length} download(s) queued`, "ok");
      };
      this.root.querySelector<HTMLButtonElement>("#tb-settings")!.onclick = openSettings;
      const pa = this.root.querySelector<HTMLButtonElement>("#tb-pause");
      const re = this.root.querySelector<HTMLButtonElement>("#tb-resume");
      if (pa) pa.onclick = () => store.downloads.filter((d) => d.status === "downloading" || d.status === "queued").forEach((d) => api.pauseDownload(d.id));
      if (re) re.onclick = async () => {
        const n = await api.resumeAllDownloads();
        toast(n ? `${n} download(s) resumed` : "Nothing to resume", n ? "ok" : "info");
      };
      const rt = this.root.querySelector<HTMLButtonElement>("#tb-retry");
      if (rt) rt.onclick = async () => {
        const n = await api.retryAllDownloads();
        toast(n ? `${n} download(s) retried` : "Nothing to retry", n ? "ok" : "info");
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
      this.root.querySelector<HTMLElement>("#list")!.addEventListener("click", (e) => {
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
        if (act === "pause") void api.pauseDownload(id);
        else if (act === "resume") void api.resumeDownload(id);
        else if (act === "folder") void api.openFolder(d.save_path);
        else if (act === "open") void api.openFile(d.save_path);
        else if (act === "reload") {
          if (d.source === "youtube") {
            const path = store.settings?.path || "";
            if (d.format_id) {
              void api.startYtdl(d.url, d.format_id, path, false, undefined);
            } else {
              void api.fetchYtdlInfo(d.url).then((info) => {
                const best = info.formats.find((f) => f.note?.includes("Best")) ?? info.formats[0];
                if (best) return api.startYtdl(d.url, best.id, path, false, undefined);
              });
            }
          } else {
            void api.startDownload(d.url, store.settings?.path || "", store.settings?.segments ?? 8);
          }
        } else if (act === "cancel") {
          if (d.status === "completed") {
            openConfirmRemove(d.filename || d.title, (del) => void api.removeDownload(id, del));
          } else {
            void api.removeDownload(id, false);
          }
        }
      });
      // Drag & drop URL import.
      document.addEventListener("dragover", (e) => e.preventDefault());
      document.addEventListener("drop", (e) => {
        e.preventDefault();
        const txt =
          e.dataTransfer?.getData("text/plain") ||
          e.dataTransfer?.getData("text/uri-list") ||
          e.dataTransfer?.getData("URL") ||
          "";
        const urls = txt
          .split(/[\s,;]+/)
          .map((u) => u.trim())
          .filter((u) => u.startsWith("http://") || u.startsWith("https://"));
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