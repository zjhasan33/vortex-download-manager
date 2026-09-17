export type DownloadStatus =
  | "queued"
  | "downloading"
  | "paused"
  | "completed"
  | "merging"
  | "error"
  | "cancelled"
  | "needs_auth";

export type CategoryId = "all" | "video" | "audio" | "document" | "program" | "zip" | "other";

export interface Download {
  id: string;
  url: string;
  filename: string;
  title: string;
  category: string;
  total_size: number;
  downloaded: number;
  speed: number;
  progress: number;
  eta: number;
  segments: number;
  connections: number;
  live?: number;
  status: DownloadStatus;
  error?: string;
  thumbnail?: string;
  source: "http" | "youtube";
  save_path: string;
  created_at: number;
  completed_at?: number | null;
  format_id?: string;
}

export interface YtdlInfo {
  title: string;
  thumbnail: string;
  duration: number;
  uploader: string;
  view_count: number;
  formats: YtdlFormat[];
  subtitles: YtdlSub[];
}

export interface YtdlSub {
  lang: string;
  label: string;
  auto: boolean;
}

export interface YtdlFormat {
  id: string;
  label: string;
  kind: "video" | "audio";
  quality: string;
  height?: number;
  fps?: number;
  ext: string;
  size: number;
  has_video: boolean;
  has_audio: boolean;
  note: string;
}

export interface ToolsStatus {
  ytdlp: boolean;
  ffmpeg: boolean;
  ytdlp_version: string;
  ffmpeg_version: string;
}

export interface UpdateToolsResult extends ToolsStatus {
  updated: boolean;
  message: string;
}

export interface AppStats {
  total_speed: number;
  active: number;
  completed: number;
  total_downloaded: number;
  segments: number;
  connections: number;
}

export interface GrabItem {
  url: string;
  filename: string;
  kind: string;
}

export interface Settings {
  path: string;
  segments: number;
  speed_limit: number;
  notifications: boolean;
  auto_start: boolean;
  delete_part: boolean;
  categorize_folders: boolean;
  max_active: number;
  auto_retries: number;
  proxy: string;
  use_cookies: boolean;
  cookies: string;
  on_complete: string;
  stop_at?: number | null;
  show_dropbox: boolean;
  clipboard_monitor: boolean;
  embed_subs: boolean;
  sub_langs: string;
  credentials: { host: string; username: string; password: string }[];
}