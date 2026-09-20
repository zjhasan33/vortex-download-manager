import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";
import type {
  Download,
  ToolsStatus,
  UpdateToolsResult,
  YtdlFormat,
  YtdlInfo,
  GrabItem,
  Settings,
  AppStats,
} from "../types";
import { toast } from "./ui";
import { playSuccess, playError, playBatch } from "./sound";

export interface ProgressPayload {
  id: string;
  downloaded: number;
  total_size: number;
  speed: number;
  progress: number;
  eta: number;
  segments?: number;
  connections?: number;
}

const events: UnlistenFn[] = [];

export interface AuthRequiredPayload {
  id: string;
  url: string;
  host: string;
}

export async function initApi() {
  if (events.length) return;
  events.push(
    await listen<ProgressPayload>("download-progress", (e) => {
      store.updateProgress(e.payload);
    }),
    await listen<{ id: string; status: Download["status"]; error?: string }>(
      "download-status",
      (e) => store.updateStatus(e.payload),
    ),
    await listen("downloads-changed", () =>
      store.refresh().catch((e) => console.error("[refresh failed]", e)),
    ),
    await listen<{ via?: string; url?: string; error?: string }>("dl-error", (e) => {
      const p = e.payload;
      const err = typeof p?.error === "string" && p.error ? p.error : "unknown error";
      toast(`Download failed to start (${p?.via || "extension"}): ${err}`, "err");
      console.error("[dl-error]", p);
    }),
    await listen<AuthRequiredPayload>("auth-required", (e) => {
      api.onAuthRequired?.(e.payload);
    }),
    await listen<{ url: string }>("playlist-clip", (e) => {
      api.onPlaylistClip?.(e.payload.url);
    }),
    await listen("grabber-open", (e) => {
      // Accept both `{ url: "…" }` and a bare string payload.
      const p = e.payload as unknown;
      const url = typeof p === "string" ? p : (p as { url?: string } | null)?.url;
      if (url) api.onGrabberOpen?.(url);
    }),
    await listen<{
      url: string;
      filename?: string;
      referer?: string;
      cookies?: string;
      ytdl?: {
        format_id: string;
        title?: string;
        size?: number;
        playlist?: boolean;
        playlist_items?: string;
        embed_subs?: boolean;
        sub_langs?: string;
        embed_thumbnail?: boolean;
        auto_subs?: boolean;
      };
    }>("download-intercept", (e) => {
      api.onIntercept?.(e.payload);
    }),
  );
}

async function cmd<T>(name: string, args?: Record<string, unknown>): Promise<T> {
  return invoke<T>(name, args);
}

export const api = {
  startDownload: (url: string, savePath: string, segments: number, filename?: string, startAt?: number, startPaused?: boolean, onExists?: string, referer?: string, cookies?: string) =>
    cmd<Download>("start_download", { url, savePath, segments, filename, startAt, startPaused, onExists, referer, cookies }),
  pauseDownload: (id: string) => cmd<void>("pause_download", { id }),
  resumeDownload: (id: string) => cmd<void>("resume_download", { id }),
  retryAllDownloads: () => cmd<number>("retry_all_downloads"),
  resumeAllDownloads: () => cmd<number>("resume_all_downloads"),
  pauseAllDownloads: () => cmd<number>("pause_all_downloads"),
  cancelAllActive: () => cmd<number>("cancel_all_active"),
  cancelDownload: (id: string) => cmd<void>("cancel_download", { id }),
  removeDownload: (id: string, deleteFile?: boolean) => cmd<void>("remove_download", { id, deleteFile }),
  downloadsAction: (action: "pause" | "resume" | "retry" | "remove", ids: string[], deleteFile?: boolean) =>
    cmd<number>("downloads_action", { action, ids, deleteFile }),
  listDownloads: () => cmd<Download[]>("list_downloads"),
  getStats: () => cmd<AppStats>("get_stats"),

  fetchYtdlInfo: (url: string, referer?: string, userAgent?: string) =>
    cmd<YtdlInfo>("fetch_ytdl_info", { url, referer, user_agent: userAgent }),
  cancelShutdown: () => cmd<void>("cancel_shutdown"),
  grabSite: (url: string, maxPages?: number, kinds?: string[]) =>
    cmd<GrabItem[]>("grab_site", { url, maxPages, kinds }),
  grabStop: () => cmd<void>("grab_stop"),  startYtdl: (
    url: string,
    formatId: string,
    savePath: string,
    playlist?: boolean,
    playlistItems?: string,
    startAt?: number,
    embedSubs?: boolean,
    subLangs?: string,
    embedThumbnail?: boolean,
    autoSubs?: boolean,
    referer?: string,
    userAgent?: string,
    cookies?: string,
    startPaused?: boolean,
    allowDup?: boolean,
  ) =>
    cmd<Download>("start_ytdl", {
      url,
      formatId,
      savePath,
      includePlaylist: playlist,
      playlistItems,
      startAt,
      embedSubs,
      subLangs,
      embed_thumbnail: embedThumbnail,
      autoSubs,
      referer,
      user_agent: userAgent,
      cookies,
      startPaused,
      allow_dup: allowDup,
    }),

  openFolder: (path: string) => cmd<void>("open_folder", { path }),
  openFile: (path: string) => cmd<void>("open_saved_file", { path }),
  readUrls: () => cmd<string[]>("read_urls"),
  probeDownloadInfo: (url: string) =>
    cmd<{ filename: string; size: number; category: string }>("probe_download_info", { url }),
  takeDialogPayload: () =>
    cmd<{ url: string; filename?: string; referer?: string; cookies?: string; ytdl?: Record<string, unknown> } | null>("take_dialog_payload"),
  ytdlExpectedPath: (dir: string, title: string, ext: string, playlist: boolean) =>
    cmd<{ exists: boolean; path: string; is_dir: boolean }>("ytdl_expected_path", { dir, title, ext, playlist }),
  deleteFileAt: (path: string) => cmd<void>("delete_file_at", { path }),
  convertToMp3: (path: string) => cmd<string>("convert_to_mp3", { path }),
  convertMedia: (path: string, format: string, quality?: string) => cmd<string>("convert_media", { path, format, quality }),
  autoOrganize: (path: string) => cmd<string | null>("auto_organize", { path }),
  getTools: () => cmd<ToolsStatus>("get_tools_status"),
  updateTools: (forceFfmpeg = false) => cmd<UpdateToolsResult>("update_tools", { forceFfmpeg }),
  getSettings: () => cmd<Settings>("get_settings"),
  saveSettings: (s: Settings) => cmd<void>("save_settings", { settings: s }),
  chooseFolder: () => cmd<string | null>("choose_folder"),
  chooseCookiesFile: () => cmd<string | null>("choose_cookies_file"),
  setAuth: (id: string, host: string, username: string, password: string, remember: boolean) =>
    cmd<number>("set_auth", { id, host, username, password, remember }),
  removeCredential: (host: string) => cmd<void>("remove_credential", { host }),
  getDownloadPath: () => cmd<string>("get_download_path"),
  getWsToken: () => cmd<string>("get_ws_token"),
  windowAction: (action: "minimize" | "toggle" | "close" | "hide" | "show") =>
    cmd<void>("window_action", { action }),
  /** Set by app.ts to open the login dialog when a download needs credentials. */
  onAuthRequired: null as ((p: AuthRequiredPayload) => void) | null,
  /** Set by app.ts to open the YouTube modal when a playlist URL is copied. */
  onPlaylistClip: null as ((url: string) => void) | null,
  /** Set by app.ts to open the Site Grabber modal (extension "Grab This Page"). */
  onGrabberOpen: null as ((url: string) => void) | null,
  /** Fired when the browser intercepts a download — show the IDM-style Start/Later/Cancel. */
  onIntercept: null as ((p: { url: string; filename?: string; referer?: string; cookies?: string; format_id?: string; is_ytdl?: boolean; ytdl?: { format_id: string; title?: string; size?: number; playlist?: boolean; playlist_items?: string; embed_subs?: boolean; sub_langs?: string; embed_thumbnail?: boolean; auto_subs?: boolean } }) => void) | null,
};

// ---- Lightweight reactive store ----
type Listener = () => void;

class Store {
  downloads: Download[] = [];
  stats: AppStats = { total_speed: 0, active: 0, completed: 0, total_downloaded: 0, segments: 0, connections: 0 };
  tools: ToolsStatus | null = null;
  settings: Settings | null = null;
  private listeners = new Set<Listener>();
  private ticking = false;
  private speedEma = new Map<string, number>();
  private totalEma = 0;

  subscribe(fn: Listener) {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }
  private emit() {
    this.listeners.forEach((fn) => fn());
  }

  async refresh() {
    const [dl, st] = await Promise.all([api.listDownloads(), api.getStats()]);
    this.downloads = dl;
    this.stats = st;
    this.emit();
  }

  updateProgress(p: ProgressPayload) {
    const d = this.downloads.find((x) => x.id === p.id);
    if (d) {
      d.downloaded = p.downloaded;
      d.total_size = p.total_size;
      // EMA smoothing (0.3*current + 0.7*prev) — only the displayed speed is smoothed, byte counters stay raw.
      const prev = this.speedEma.get(p.id) ?? p.speed;
      const smoothed = p.speed * 0.3 + prev * 0.7;
      this.speedEma.set(p.id, smoothed);
      // Prune stale ids (completed downloads keep map bounded).
      if (this.speedEma.size > 64) {
        for (const k of this.speedEma.keys()) {
          if (!this.downloads.some((x) => x.id === k)) this.speedEma.delete(k);
          if (this.speedEma.size <= 32) break;
        }
      }
      d.speed = Math.round(smoothed);
      d.progress = p.progress;
      d.eta = p.eta;
      if (p.segments != null) d.segments = p.segments;
      if (p.connections != null) d.live = p.connections;
    }
    // Live stats from in-flight payloads so the statusbar moves in real time
    // instead of waiting for the next get_stats poll (which still refreshes
    // completed counts, totals, etc).
    let speed = 0,
      active = 0,
      segs = 0,
      conns = 0;
    for (const x of this.downloads) {
      if (x.status === "downloading" || x.status === "merging" || x.status === "resolving") {
        active += 1;
        speed += x.speed;
        segs += x.segments;
        conns += x.live || 0;
      }
    }
    this.totalEma = speed * 0.3 + this.totalEma * 0.7;
    if (this.totalEma === 0) this.totalEma = speed;
    // Use smoothed total when active, raw when idle (avoids ghost speed).
    const displayTotal = active ? Math.round(this.totalEma) : speed;
    if (!active) this.totalEma = 0;
    this.stats = { ...this.stats, total_speed: displayTotal, active, segments: segs, connections: conns };
    this.emit();
  }

  updateStatus(p: { id: string; status: Download["status"]; error?: string }) {
    const d = this.downloads.find((x) => x.id === p.id);
    const prev = d?.status;
    if (d) {
      d.status = p.status;
      if (p.error) d.error = p.error;
    }
    // Notification sounds (Settings → Notification Sounds, default on).
    // Only on real transitions to avoid double-chimes from re-renders.
    if (prev !== p.status && (this.settings?.sounds ?? true)) {
      if (p.status === "completed") {
        const busy = this.downloads.some(
          (x) =>
            x.id !== p.id &&
            (x.status === "downloading" ||
              x.status === "merging" ||
              x.status === "queued" ||
              x.status === "resolving"),
        );
        if (busy) playSuccess();
        else playBatch();
      } else if (p.status === "error") {
        playError();
      }
    }
    this.emit();
    // Background refresh: failures stay in the console (a toast here would
    // spam on every event while the backend is down).
    this.refresh().catch((e) => console.error("[refresh failed]", e));
  }
}

export const store = new Store();
export function startTicker(fps = 4) {
  setInterval((): void => {
    if (
      store.downloads.some(
        (d) => d.status === "downloading" || d.status === "merging" || d.status === "resolving",
      )
    ) {
      store.refresh().catch((e) => console.error("[ticker refresh failed]", e));
    }
  }, 1000 / fps);
}