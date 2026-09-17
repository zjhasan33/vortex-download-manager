export type IconName =
  | "logo" | "add" | "pause" | "play" | "stop" | "trash" | "folder"
  | "search" | "youtube" | "gear" | "download" | "close" | "video"
  | "audio" | "doc" | "program" | "zip" | "other" | "check" | "speed"
  | "link" | "minimize" | "maximize" | "sync" | "file" | "bell" | "redo"
  | "copy" | "key";

const PATHS: Record<IconName, string> = {
  logo: `<path d="M12 2 3 7v10l9 5 9-5V7l-9-5z"/><path d="M12 22V12" stroke="#06080f" stroke-width="1.6"/><path d="M3 7l9 5 9-5M12 12v10" stroke="#06080f" stroke-width="1.6"/>`,
  add: `<path d="M12 5v14M5 12h14" stroke-linecap="round" stroke-width="2"/>`,
  pause: `<path d="M10 5v14M14 5v14" stroke-linecap="round" stroke-width="2.4"/>`,
  play: `<path d="M8 5l11 7-11 7V5z" stroke-linejoin="round" stroke-width="2"/>`,
  stop: `<rect x="7" y="7" width="10" height="10" rx="2"/>`,
  trash: `<path d="M4 7h16M9 7V5a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2m3 0-1 13a2 2 0 0 1-2 2H9a2 2 0 0 1-2-2L6 7M10 11v6M14 11v6" stroke-linecap="round" stroke-width="1.7"/>`,
  folder: `<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V7z" stroke-linejoin="round"/>`,
  search: `<circle cx="11" cy="11" r="6"/><path d="M20 20l-4.5-4.5" stroke-linecap="round"/>`,
  youtube: `<rect x="2.5" y="6" width="19" height="12" rx="3.5"/><path d="M10.5 9.5v5l4.5-2.5-4.5-2.5z" fill="#fff"/>`,
  gear: `<circle cx="12" cy="12" r="3.2"/><path d="M19 12a7 7 0 0 0-.1-1.2l2-1.5-2-3.5-2.4 1a7 7 0 0 0-2-1.2L14 3h-4l-.5 2.6a7 7 0 0 0-2 1.2l-2.4-1-2 3.5 2 1.5a7 7 0 0 0 0 2.4l-2 1.5 2 3.5 2.4-1a7 7 0 0 0 2 1.2l.5 2.6h4l.5-2.6a7 7 0 0 0 2-1.2l2.4 1 2-3.5-2-1.5c.1-.4.1-.8.1-1.2z" stroke-linejoin="round"/>`,
  download: `<path d="M12 3v11m0 0 4.5-4.5M12 14 7.5 9.5" stroke-linecap="round" stroke-linejoin="round"/><path d="M4 17v2a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-2" stroke-linecap="round"/>`,
  close: `<path d="M6 6l12 12M18 6L6 18" stroke-linecap="round" stroke-width="2"/>`,
  video: `<rect x="3" y="5" width="13" height="14" rx="2.5"/><path d="M16 10l5-3v10l-5-3V10z" stroke-linejoin="round"/>`,
  audio: `<path d="M9 18V6l10-2v11" stroke-linecap="round" stroke-linejoin="round"/><circle cx="6.5" cy="18" r="2.5"/><circle cx="16.5" cy="15" r="2.5"/>`,
  doc: `<path d="M6 2h8l4 4v16H6V2z" stroke-linejoin="round"/><path d="M14 2v4h4M10 11h6M10 15h6M10 8h2" stroke-linecap="round"/>`,
  program: `<rect x="4" y="4" width="16" height="16" rx="2.5"/><path d="M8.5 10l-2 2 2 2M15.5 10l2 2-2 2M13 8l-2 8" stroke-linecap="round" stroke-linejoin="round"/>`,
  zip: `<rect x="4" y="3.5" width="16" height="17" rx="2.5"/><path d="M8 3.5l4 5v11h-4a2 2 0 0 1-2-2v-4l4-5 1 4h-3M19 4.5" stroke-linecap="round" stroke-linejoin="round"/>`,
  other: `<circle cx="12" cy="12" r="8.5"/><path d="M12 7.5v9M7.5 12h9" stroke-linecap="round"/>`,
  check: `<path d="M5 12.5l4.5 4.5L19 7" stroke-linecap="round" stroke-linejoin="round" stroke-width="2.2"/>`,
  speed: `<path d="M12 3a9 9 0 0 1 9 9v3h-4v-3a5 5 0 0 0-10 0v3H3v-3a9 9 0 0 1 9-9z"/><path d="M12 14l3-3" stroke-linecap="round"/>`,
  link: `<path d="M10 14a4 4 0 0 0 6 .7l3-3a4 4 0 0 0-5.7-5.7l-1.3 1.3" stroke-linecap="round"/><path d="M14 10a4 4 0 0 0-6-.7l-3 3a4 4 0 0 0 5.7 5.7l1.3-1.3" stroke-linecap="round"/>`,
  minimize: `<path d="M5 12h14" stroke-width="1.6" stroke-linecap="round"/>`,
  maximize: `<rect x="5" y="5" width="14" height="14" rx="2"/>`,
  sync: `<path d="M20 11a8 8 0 0 0-14-4.5L4 9m0-5v5h5M4 13a8 8 0 0 0 14 4.5L20 15m0 5v-5h-5" stroke-linecap="round" stroke-linejoin="round"/>`,
  file: `<path d="M5 21h14V7l-6-4H5v18z" stroke-linejoin="round"/><path d="M13 3v5h6" stroke-linejoin="round"/><path d="M8.5 13h7M8.5 17h4" stroke-linecap="round"/>`,
  copy: `<rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V5a2 2 0 0 1 2-2h10" stroke-linecap="round" stroke-linejoin="round"/>`,
  key: `<circle cx="7.5" cy="15.5" r="4.5"/><path d="M10.8 12.2 20 3M15 8l3 3M17.5 5.5l2 2" stroke-linecap="round" stroke-linejoin="round"/>`,
  bell: `<path d="M6 16v-5a6 6 0 1 1 12 0v5l2 2H4l2-2z" stroke-linejoin="round"/><path d="M10 20a2 2 0 0 0 4 0" stroke-linecap="round"/>`,
  redo: `<path d="M3 8a5 5 0 0 1 8.5-3.5L13 6m5 0c-3.5-3.5-9-3.5-11 2M18 4v4h-4M21 16a5 5 0 0 1-8.5 3.5L11 18m-5 0c3.5 3.5 9 3.5 11-2M6 20v-4h4" stroke-linecap="round" stroke-linejoin="round"/>`,
};

export function icon(name: IconName, size = 20): string {
  return `<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">${PATHS[name]}</svg>`;
}

export function catIcon(cat: string, size = 20): string {
  const map: Record<string, IconName> = {
    video: "video",
    audio: "audio",
    document: "doc",
    program: "program",
    zip: "zip",
  };
  return icon(map[cat] ?? "other", size);
}