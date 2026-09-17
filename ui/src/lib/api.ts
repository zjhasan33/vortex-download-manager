import { invoke } from "@tauri-apps/api/core";
import { listen, UnlistenFn } from "@tauri-apps/api/event";
import type {
  Download,
  ToolsStatus,
  YtdlFormat,
  YtdlInfo,
  GrabItem,
  Settings,
  AppStats,
} from "../types";

export interface ProgressPayload {
  id: string;
  downloaded: number;
  total_size: number;
  speed: number;
  progress: number;
  eta: number;
}

const events: UnlistenFn[] = [];

export interface AuthRequiredPayload {
  id: string;
  url: string;
  host: string;
}

/** Set by app.ts to open the login dialog when a download needs credentials. */
export let onAuthRequired: ((p: AuthRequiredPayload) => void) | null = null;

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
    await listen("downloads-changed", () => store.refresh()),
    await listen<AuthRequiredPayload>("auth-required", (e) => {
      onAuthRequired?.(e.payload);
    }),
  );
}

async function cmd<T>(name: string, args?: Record<string, unknown>): Promise<T> {
  return invoke<T>(name, args);
}

export const api = {
  startDownload: (url: string, savePath: string, segments: number, filename?: string, startAt?: number) =>
    cmd<Download>("start_download", { url, savePath, segments, filename, startAt }),
  pauseDownload: (id: string) => cmd<void>("pause_download", { id }),
  resumeDownload: (id: string) => cmd<void>("resume_download", { id }),
  retryAllDownloads: () => cmd<number>("retry_all_downloads"),
  resumeAllDownloads: () => cmd<number>("resume_all_downloads"),
  cancelDownload: (id: string) => cmd<void>("cancel_download", { id }),
  removeDownload: (id: string, deleteFile?: boolean) => cmd<void>("remove_download", { id, deleteFile }),
  downloadsAction: (action: "pause" | "resume" | "retry" | "remove", ids: string[], deleteFile?: boolean) =>
    cmd<number>("downloads_action", { action, ids, deleteFile }),
  listDownloads: () => cmd<Download[]>("list_downloads"),
  getStats: () => cmd<AppStats>("get_stats"),

  fetchYtdlInfo: (url: string) => cmd<YtdlInfo>("fetch_ytdl_info", { url }),
  grabSite: (url: string, maxPages?: number, kinds?: string[]) =>
    cmd<GrabItem[]>("grab_site", { url, maxPages, kinds }),  startYtdl: (
    url: string,
    formatId: string,
    savePath: string,
    playlist?: boolean,
    playlistItems?: string,
    startAt?: number,
    embedSubs?: boolean,
    subLangs?: string,
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
    }),

  openFolder: (path: string) => cmd<void>("open_folder", { path }),
  openFile: (path: string) => cmd<void>("open_saved_file", { path }),
  readUrls: () => cmd<string[]>("read_urls"),
  getTools: () => cmd<ToolsStatus>("get_tools_status"),
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
};

// ---- Lightweight reactive store ----
type Listener = () => void;

class Store {
  downloads: Download[] = [];
  stats: AppStats = { total_speed: 0, active: 0, completed: 0, total_downloaded: 0, segments: 0 };
  tools: ToolsStatus | null = null;
  settings: Settings | null = null;
  private listeners = new Set<Listener>();
  private ticking = false;

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
      d.speed = p.speed;
      d.progress = p.progress;
      d.eta = p.eta;
    }
    this.emit();
  }

  updateStatus(p: { id: string; status: Download["status"]; error?: string }) {
    const d = this.downloads.find((x) => x.id === p.id);
    if (d) {
      d.status = p.status;
      if (p.error) d.error = p.error;
    }
    this.emit();
    this.refresh();
  }
}

export const store = new Store();
export function startTicker(fps = 4) {
  setInterval((): void => {
    if (store.downloads.some((d) => d.status === "downloading" || d.status === "merging")) {
      store.refresh();
    }
  }, 1000 / fps);
}