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

function youtubeId(url: string): string | null {
  try {
    const u = url.toLowerCase();
    const m1 = u.match(/[?&]v=([^&#]+)/);
    if (m1) return m1[1];
    const m2 = u.match(/youtu\.be\/([^?&#/]+)/);
    if (m2) return m2[1];
    const m3 = u.match(/\/(embed|v|shorts)\/([^?&#/]+)/);
    if (m3) return m3[2];
  } catch {}
  return null;
}

async function openFromPayload(p: DialogPayload) {
  if (!p || !p.url) return;
  // Check A: URL / YouTube ID already in history (including Completed).
  // Format-aware: exact same format/extension only (MP4 vs MP3, 720p vs 1080p are different).
  const newFmt = p.ytdl?.format_id || "";
  try {
    const id = youtubeId(p.url);
    const list: { url: string; save_path: string; filename: string; format_id?: string }[] = await api.listDownloads().catch(() => []);
    const dup = list.find((d) => {
      if (d.url === p.url) {
        const dupFmt = (d as unknown as { format_id?: string }).format_id || "";
        if (newFmt && dupFmt && dupFmt !== newFmt) return false;
        return true;
      }
      if (id) {
        const did = youtubeId(d.url);
        if (did && did.toLowerCase() === id.toLowerCase()) {
          const dupFmt = (d as unknown as { format_id?: string }).format_id || "";
          if (newFmt && dupFmt && dupFmt !== newFmt) return false;
          return true;
        }
      }
      return false;
    });
    if (dup) {
      const wantAgain = await new Promise<boolean>((res) => {
        // Direct modal for "already downloaded" — Download Again / Cancel (IDM parity).
        import("./components/modals").then((m) => {
          // Use the existing file-exists modal but with tailored text.
          const anyMod = m as unknown as Record<string, unknown>;
          const fn = (anyMod.openConfirmExists as ((path: string, cb: (a: string) => void) => void) | undefined)
            || (anyMod.openConfirmRemove as unknown as ((path: string, cb: (a: string) => void) => void));
          if (fn) {
            const label = (dup.save_path || (dup as unknown as { filename: string }).filename || p.url) + " — already downloaded. Download again?";
            fn(label, (choice: string) => res(choice !== "cancel"));
          } else {
            res(window.confirm("This video/file has already been downloaded! Download again?"));
          }
        }).catch(() => res(window.confirm("This video/file has already been downloaded! Download again?")));
      });
      if (!wantAgain) {
        getCurrentWindow().close().catch(() => {});
        return;
      }
    }
  } catch {}
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
