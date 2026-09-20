import { getCurrentWindow } from "@tauri-apps/api/window";
import { listen } from "@tauri-apps/api/event";
import { api, initApi, store } from "./lib/api";
import { openIntercept } from "./components/modals";
import { toast } from "./lib/ui";

interface DialogPayload {
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
}

function openFromPayload(p: DialogPayload) {
  if (!p || !p.url) return;
  // Backend may use "referer" or "referrer" spelling; accept both.
  const raw = (p as unknown as Record<string, unknown>)["referrer"];
  const referer =
    (typeof p.referer === "string" && p.referer) ||
    (typeof raw === "string" && raw) ||
    undefined;
  const y = p.ytdl;
  openIntercept(p.url, p.filename || undefined, referer, p.cookies, {
    title: y?.title,
    size: y?.size,
    isYtdl: !!y,
    ytdl: y
      ? {
          format_id: y.format_id,
          playlist: y.playlist,
          playlist_items: y.playlist_items,
          embed_subs: y.embed_subs,
          sub_langs: y.sub_langs,
          embed_thumbnail: y.embed_thumbnail,
          auto_subs: y.auto_subs,
        }
      : undefined,
    onDone: () => {
      // Last modal in a standalone dialog window: close the window itself.
      getCurrentWindow()
        .close()
        .catch(() => {});
    },
  });
}

/** Standalone "Download File Info" window (#/download-info route). */
export async function bootDownloadInfo() {
  const showErr = (m: string) => {
    document.body.innerHTML = `<div style="color:#fff;padding:24px;font:14px sans-serif">Dialog boot failed:<br><br><span style="color:#f88">${m}</span></div>`;
  };
  try {
    await initApi();
  } catch (e) {
    showErr("initApi: " + String(e));
    return;
  }
  try {
    store.settings = await api.getSettings();
  } catch {
    /* defaults */
  }
  document.body.classList.add("infowin");
  window.addEventListener("error", (e) => toast("UI error: " + e.message, "err"));

  // A second download while the dialog is open stacks another modal.
  try {
    await listen<DialogPayload>("dialog-update", (e) => {
      if (e.payload?.url) openFromPayload(e.payload);
    });
  } catch (e) {
    showErr("listen: " + String(e));
    return;
  }

  let payload: DialogPayload | null = null;
  try {
    payload = (await api.takeDialogPayload().catch(() => null)) as DialogPayload | null;
  } catch (e) {
    showErr("takeDialogPayload: " + String(e));
    return;
  }
  if (!payload || !payload.url) {
    showErr("empty payload (backend handed nothing)");
    return;
  }
  try {
    openFromPayload(payload as DialogPayload);
  } catch (e) {
    showErr("openIntercept: " + String(e));
  }
}
