export type DownloadStatus =
  | "queued"
  | "downloading"
  | "paused"
  | "completed"
  | "merging"
  | "error"
  | "cancelled";

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
  status: DownloadStatus;
  error?: string;
  thumbnail?: string;
  source: "http" | "youtube";
  save_path: string;
  created_at: number;
}

export interface YtdlInfo {
  title: string;
  thumbnail: string;
  duration: number;
  uploader: string;
  view_count: number;
  formats: YtdlFormat[];
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

export interface AppStats {
  total_speed: number;
  active: number;
  completed: number;
  total_downloaded: number;
  segments: number;
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
}